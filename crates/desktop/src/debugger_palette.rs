//! Form fields derived from the wire schema. Dispatch always goes through the typed command.
use std::sync::OnceLock;

use lotus_protocol::{debug::DebugCommand, debug_schema};
use serde_json::{Map, Value};

use crate::debugger_coverage::INVENTORY;

pub(crate) const COMMAND_LIMIT: usize = 64 << 10;

pub(crate) enum Kind {
    Text,
    Integer { signed: bool },
    Choice(Vec<String>),
    Boolean,
    Json,
}

pub(crate) struct Field {
    pub name: String,
    pub kind: Kind,
    pub required: bool,
    pub default: Option<Value>,
    pub description: String,
}

impl Field {
    pub(crate) fn hint(&self) -> &'static str {
        match &self.kind {
            Kind::Text => "Text (enter \"\" for an empty string)",
            Kind::Integer { .. } => "Decimal or 0x hex",
            Kind::Choice(_) => "Choose a value",
            Kind::Boolean => "Choose true or false",
            Kind::Json => "JSON value",
        }
    }

    fn parse(&self, text: &str) -> Result<Option<Value>, String> {
        if text.is_empty() {
            return if self.required {
                Err("Required".into())
            } else {
                Ok(None)
            };
        }
        let value = match &self.kind {
            Kind::Text => Value::String(if text == "\"\"" {
                String::new()
            } else {
                text.into()
            }),
            Kind::Integer { signed } => parse_integer(text.trim(), *signed)?,
            Kind::Choice(choices) => {
                if !choices.iter().any(|choice| choice == text) {
                    return Err("Choose one of the listed values".into());
                }
                Value::String(text.into())
            }
            Kind::Boolean => Value::Bool(text.parse().map_err(|_| "Expected true or false")?),
            Kind::Json => serde_json::from_str(text).map_err(|error| error.to_string())?,
        };
        Ok(Some(value))
    }
}

pub(crate) fn parse_integer(text: &str, signed: bool) -> Result<Value, String> {
    let error = || "Expected an in-range decimal or 0x hexadecimal integer".to_string();
    let (negative, digits) = text.strip_prefix('-').map_or((false, text), |n| (true, n));
    let (radix, digits) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
        .map_or((10, digits), |hex| (16, hex));
    if digits.starts_with('+') {
        return Err(error());
    }
    let bits = u64::from_str_radix(digits, radix).map_err(|_| error())?;
    match (signed, negative) {
        (false, false) => Ok(Value::from(bits)),
        (false, true) => Err(error()),
        (true, _) => {
            let signed = if negative {
                -i128::from(bits)
            } else {
                i128::from(bits)
            };
            Ok(Value::from(i64::try_from(signed).map_err(|_| error())?))
        }
    }
}

pub(crate) struct Operation {
    pub name: String,
    pub group: &'static str,
    pub route: &'static str,
    pub permission: &'static str,
    pub feature: &'static str,
    pub note: &'static str,
    pub fields: Vec<Field>,
}

impl Operation {
    pub(crate) fn parse(&self, arguments: &[&str]) -> Result<DebugCommand, String> {
        if arguments.len() != self.fields.len() {
            return Err("Argument form is incomplete".into());
        }
        if arguments
            .iter()
            .map(|argument| argument.len())
            .sum::<usize>()
            > COMMAND_LIMIT
        {
            return Err("Command exceeds 64 KiB".into());
        }
        let mut object = Map::new();
        object.insert("operation".into(), Value::String(self.name.clone()));
        for (field, text) in self.fields.iter().zip(arguments) {
            if let Some(value) = field
                .parse(text)
                .map_err(|e| format!("{} · {e}", field.name))?
            {
                object.insert(field.name.clone(), value);
            }
        }
        let command =
            serde_json::from_value(Value::Object(object)).map_err(|error| error.to_string())?;
        validate(command)
    }

    pub(crate) fn matches(&self, query: &str) -> bool {
        query
            .split_whitespace()
            .all(|word| self.name.contains(word) || self.group.to_ascii_lowercase().contains(word))
    }
}

pub(crate) fn parse_json(text: &str) -> Result<DebugCommand, String> {
    if text.len() > COMMAND_LIMIT {
        return Err("Command exceeds 64 KiB".into());
    }
    let command = serde_json::from_str(text).map_err(|error| error.to_string())?;
    // Serde accepts unknown fields on unit variants even with deny_unknown_fields.
    // Check their property names against the same contract as the generated forms.
    let object: Map<String, Value> =
        serde_json::from_str(text).map_err(|error| error.to_string())?;
    let name = object["operation"]
        .as_str()
        .ok_or("Expected an operation")?;
    let operation = catalogue()
        .iter()
        .find(|op| op.name == name)
        .ok_or("Unknown debugger operation")?;
    if let Some(name) = object.keys().find(|key| {
        *key != "operation" && !operation.fields.iter().any(|field| field.name == **key)
    }) {
        return Err(format!("Unknown argument · {name}"));
    }
    validate(command)
}

fn validate(command: DebugCommand) -> Result<DebugCommand, String> {
    command.validate()?;
    let payload = serde_json::to_vec(&command).map_err(|error| error.to_string())?;
    if payload.len() > COMMAND_LIMIT {
        return Err("Command exceeds 64 KiB".into());
    }
    Ok(command)
}

pub(crate) fn operation(command: &DebugCommand) -> Option<&'static Operation> {
    let value = serde_json::to_value(command).ok()?;
    let name = value["operation"].as_str()?;
    catalogue().iter().find(|operation| operation.name == name)
}

pub(crate) fn catalogue() -> &'static [Operation] {
    static CATALOGUE: OnceLock<Vec<Operation>> = OnceLock::new();
    CATALOGUE.get_or_init(|| {
        let schema = debug_schema::schema();
        let mut catalogue = Vec::new();
        for variant in schema["oneOf"].as_array().expect("command union") {
            let properties = variant["properties"]
                .as_object()
                .expect("command properties");
            let name = properties["operation"]["const"]
                .as_str()
                .expect("operation tag");
            let required = variant["required"]
                .as_array()
                .expect("required operation tag");
            let [_, _, route, permission, feature, note, _] = INVENTORY
                .lines()
                .find_map(|line| {
                    let fields: [_; 7] = line.split('\t').collect::<Vec<_>>().try_into().ok()?;
                    (fields[0] == "debug" && fields[1] == name).then_some(fields)
                })
                .unwrap_or_else(|| panic!("missing coverage for {name}"));
            let mut fields = properties
                .iter()
                .filter(|(name, _)| *name != "operation")
                .map(|(name, property)| Field {
                    name: name.clone(),
                    kind: field_kind(&schema, property),
                    required: required.iter().any(|key| key.as_str() == Some(name)),
                    default: property.get("default").cloned(),
                    description: property["description"].as_str().unwrap_or("").into(),
                })
                .collect::<Vec<_>>();
            fields.sort_by_key(|field| !field.required);
            catalogue.push(Operation {
                name: name.into(),
                group: group(name),
                route,
                permission,
                feature,
                note,
                fields,
            });
        }
        catalogue.sort_by_key(|op| (op.group, op.name.clone()));
        catalogue
    })
}

fn field_kind(schema: &Value, property: &Value) -> Kind {
    if let Some(reference) = property["$ref"]
        .as_str()
        .and_then(|path| path.strip_prefix('#'))
    {
        return field_kind(schema, schema.pointer(reference).expect("local definition"));
    }
    if let Some(options) = property["enum"].as_array() {
        return Kind::Choice(
            options
                .iter()
                .map(|v| v.as_str().expect("string enum").into())
                .collect(),
        );
    }
    if let Some(choices) = property["oneOf"].as_array().and_then(|options| {
        options
            .iter()
            .map(|option| option["const"].as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
    }) {
        return Kind::Choice(choices);
    }
    if let Some(options) = property["anyOf"].as_array() {
        if let Some(option) = options.iter().find(|option| option["type"] != "null") {
            if options
                .iter()
                .filter(|option| option["type"] != "null")
                .count()
                == 1
            {
                return field_kind(schema, option);
            }
        }
    }
    let kind = property["type"].as_str().or_else(|| {
        property["type"]
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .find(|kind| *kind != "null")
    });
    match kind {
        Some("string") => Kind::Text,
        Some("integer") => Kind::Integer {
            signed: !property["format"]
                .as_str()
                .is_some_and(|format| format.starts_with("uint")),
        },
        Some("boolean") => Kind::Boolean,
        _ => Kind::Json,
    }
}

fn group(name: &str) -> &'static str {
    match name {
        "advance" | "continue" | "pause" | "reset" | "reverse_continue" | "reverse_step"
        | "run_to" | "run_until" | "step" | "step_out" | "step_over" | "stop" | "suspend"
        | "load_state" | "save_state" | "states" | "rewind_configure" | "rewind_status"
        | "movie_play" | "movie_record" | "movie_status" | "movie_stop" => "Execution",
        "breakpoint_configure"
        | "breakpoint_remove"
        | "breakpoint_set"
        | "breakpoints"
        | "backtrace"
        | "crash_report"
        | "disassemble"
        | "load_symbols"
        | "registers"
        | "resolve_symbol"
        | "symbolize"
        | "thread_registers"
        | "threads"
        | "write_register"
        | "write_thread_register" => "CPU",
        "memory_map" | "pointer_chain" | "read_memory" | "scan" | "scan_narrow"
        | "scan_results" | "write_memory" => "Memory",
        "capture_scaled" | "gpu_configure" | "gpu_draw" | "gpu_draws" | "shader_trace"
        | "surface" => "GPU",
        "citrace_replay" | "citrace_start" | "citrace_status" | "citrace_stop" | "events"
        | "events_configure" | "logs" | "trace" | "state_hash" => "Tracing",
        "audio_capture" | "audio_focus" | "fork" | "lease_acquire" | "lease_release" | "load"
        | "matrix" | "perf" | "settings_diff" | "settings_get" | "settings_list"
        | "settings_set" | "status" => "Host",
        _ => "Guest system",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(name: &str, args: &[(&str, &str)]) -> Result<DebugCommand, String> {
        let operation = catalogue().iter().find(|op| op.name == name).unwrap();
        let text = operation
            .fields
            .iter()
            .map(|field| {
                args.iter()
                    .find_map(|(name, value)| (*name == field.name).then_some(*value))
                    .unwrap_or("")
            })
            .collect::<Vec<_>>();
        operation.parse(&text)
    }

    #[test]
    fn catalogue_covers_shared_wire_schema() {
        let mut names: Vec<_> = catalogue().iter().map(|op| op.name.clone()).collect();
        names.sort();
        assert_eq!(names, debug_schema::operations());
        let external: Vec<_> = catalogue()
            .iter()
            .filter(|op| op.route == "external only")
            .map(|op| op.name.as_str())
            .collect();
        assert_eq!(external, ["lease_acquire", "lease_release"]);
        assert!(catalogue()
            .iter()
            .any(|op| op.name == "read_memory" && op.matches("read memory")));
    }

    #[test]
    fn forms_validate_addresses_counts_enums_and_structured_fields() {
        for address in ["0x00010000", "0X10000", "65536", "065536"] {
            assert!(matches!(
                form("read_memory", &[("address", address), ("length", "16")]).unwrap(),
                DebugCommand::ReadMemory {
                    address: 65536,
                    length: 16
                }
            ));
        }
        for address in [
            "-1",
            "0x-1",
            "0x100000000",
            "1.2",
            "0x",
            "0x+1",
            "4294967295",
        ] {
            assert!(
                form("read_memory", &[("address", address), ("length", "16")]).is_err(),
                "{address}"
            );
        }
        assert!(matches!(
            form("pointer_chain", &[("base", "0x10000"), ("offsets", "[-4,8]")]).unwrap(),
            DebugCommand::PointerChain { base: 65536, offsets } if offsets == [-4, 8]
        ));
        assert!(form(
            "pointer_chain",
            &[("base", "1"), ("offsets", "[4294967296]")]
        )
        .is_err());
        assert!(matches!(
            form(
                "gpu_configure",
                &[
                    ("enabled", "true"),
                    ("stop_events", "[]"),
                    ("capture", "draws")
                ]
            )
            .unwrap(),
            DebugCommand::GpuConfigure {
                enabled: true,
                capture: Some(lotus_protocol::debug::GpuCapture::Draws),
                ..
            }
        ));
        assert!(form(
            "gpu_configure",
            &[
                ("enabled", "true"),
                ("stop_events", "[]"),
                ("capture", "bad")
            ]
        )
        .is_err());
        assert!(matches!(
            form("settings_list", &[]).unwrap(),
            DebugCommand::SettingsList {
                offset: 0,
                limit: 32
            }
        ));
        assert!(form("settings_list", &[("limit", "33")]).is_err());
    }

    #[test]
    fn advanced_json_shares_the_typed_parser_and_payload_limit() {
        assert!(matches!(
            parse_json(r#"{"operation":"status"}"#).unwrap(),
            DebugCommand::Status
        ));
        assert!(parse_json(r#"{"operation":"status","unexpected":1}"#).is_err());
        assert!(parse_json(r#"{"operation":"read_memory","address":0,"length":0}"#).is_err());
        assert!(parse_json(&" ".repeat(COMMAND_LIMIT + 1)).is_err());
    }
}
