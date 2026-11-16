//! Owned inspector rows. Parsing, formatting and trace loading run off the GPUI thread.
use lotus_protocol::debug::DebugCommand as D;
use serde_json::Value;
use std::{io, path::Path, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Panel {
    Cpu,
    Threads,
    Breakpoints,
    Memory,
    Settings,
    Ipc,
    Logs,
    Gpu,
    Audio,
    States,
    Timeline,
    Result,
}
impl Panel {
    pub const ALL: [Self; 12] = [
        Self::Cpu,
        Self::Threads,
        Self::Breakpoints,
        Self::Memory,
        Self::Settings,
        Self::Ipc,
        Self::Logs,
        Self::Gpu,
        Self::Audio,
        Self::States,
        Self::Timeline,
        Self::Result,
    ];
    pub fn at(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::Cpu)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Threads => "Threads",
            Self::Breakpoints => "Breakpoints",
            Self::Memory => "Memory",
            Self::Settings => "Settings",
            Self::Ipc => "IPC",
            Self::Logs => "Logs",
            Self::Gpu => "GPU",
            Self::Audio => "Audio",
            Self::States => "Snapshots",
            Self::Timeline => "Timeline",
            Self::Result => "Result",
        }
    }
    pub fn command(self, offset: u32) -> Option<D> {
        Some(match self {
            Self::Cpu => D::Registers { core: 0 },
            Self::Threads => D::Threads,
            Self::Breakpoints => D::Breakpoints,
            Self::Memory => D::MemoryMap,
            Self::Settings => D::SettingsList { offset, limit: 32 },
            Self::Ipc | Self::Timeline => D::Events {
                after: u64::from(offset),
                limit: 128,
            },
            Self::Logs => D::Logs {
                after: u64::from(offset),
                limit: 128,
            },
            Self::Gpu => D::GpuDraws,
            Self::Audio => D::DspInspect,
            Self::States => D::States,
            Self::Result => return None,
        })
    }
}
#[derive(Debug, Clone)]
pub struct Row {
    pub lane: &'static str,
    pub key: String,
    pub value: String,
    pub action: Option<D>,
}
#[derive(Debug, Clone)]
pub struct Inspection {
    pub rows: Arc<Vec<Row>>,
    pub source: String,
    pub next: Option<u32>,
    pub lost: u64,
}
const ROW_CAP: usize = 100_000;
const TEXT_BUDGET: usize = 32 << 20;
fn clipped(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
fn lane(event: &Value) -> &'static str {
    match event["kind"].as_str().unwrap_or("") {
        "pica" => "GPU",
        "ipc" | "svc_enter" | "svc_exit" => "kernel",
        "guest_log" | "hio_stdout" | "native_log" => "log",
        _ => "CPU",
    }
}
impl Inspection {
    pub fn from_value(value: &Value, panel: Panel) -> Self {
        let mut rows = Vec::new();
        let mut budget = TEXT_BUDGET;
        fn flatten(
            value: &Value,
            key: String,
            rows: &mut Vec<Row>,
            budget: &mut usize,
            depth: u32,
        ) {
            if rows.len() >= 8192 || *budget < 2048 {
                return;
            }
            if depth < 10 {
                match value {
                    Value::Object(map) => {
                        for (name, value) in map {
                            flatten(value, format!("{key}.{name}"), rows, budget, depth + 1);
                        }
                        return;
                    }
                    Value::Array(values) => {
                        for (i, value) in values.iter().enumerate() {
                            flatten(value, format!("{key}[{i}]"), rows, budget, depth + 1);
                        }
                        return;
                    }
                    _ => {}
                }
            }
            let key = clipped(&key, 256);
            let text = if key.contains("registers[") || key.ends_with(".cpsr") {
                crate::lotus_window::hex_string(value)
            } else {
                clipped(&value.to_string(), 1024)
            };
            *budget -= key.len() + text.len();
            rows.push(Row {
                lane: "state",
                key,
                value: text,
                action: None,
            });
        }
        if panel == Panel::Cpu {
            if let Some(registers) = value["registers"].as_array() {
                for (i, r) in registers.iter().enumerate() {
                    rows.push(Row {
                        lane: "CPU",
                        key: format!("r{i}"),
                        value: crate::lotus_window::hex_string(r),
                        action: None,
                    });
                }
            }
            if let Some(assembly) = value["disassembly"]["assembly"].as_str() {
                for line in assembly
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .take(100)
                {
                    rows.push(Row {
                        lane: "CPU",
                        key: "instruction".into(),
                        value: line.into(),
                        action: None,
                    });
                }
            }
        }
        if matches!(panel, Panel::Timeline | Panel::Logs | Panel::Ipc) {
            if let Some(events) = value["events"].as_array() {
                for event in events {
                    if panel == Panel::Ipc && event["kind"] != "ipc" {
                        continue;
                    }
                    rows.push(Self::event(event));
                }
            }
        } else if panel == Panel::Settings {
            if let Some(settings) = value["settings"].as_array() {
                for setting in settings {
                    let name = setting["name"].as_str().unwrap_or("");
                    let v = &setting["value"];
                    rows.push(Row {
                        lane: "setting",
                        key: name.into(),
                        value: format!(
                            "{} · {} · {}",
                            v, setting["apply"], setting["consumer_sources"]
                        ),
                        action: v.as_bool().map(|v| D::SettingsSet {
                            name: name.into(),
                            value: lotus_protocol::debug::SettingValue::Boolean(!v),
                        }),
                    });
                }
            }
        } else if panel == Panel::States {
            if let Some(names) = value["states"].as_array() {
                for name in names.iter().filter_map(Value::as_str) {
                    rows.push(Row {
                        lane: "snapshot",
                        key: name.into(),
                        value: "Restore checkpoint (pauses execution)".into(),
                        action: Some(D::LoadState { name: name.into() }),
                    });
                }
            }
        } else if panel == Panel::Gpu {
            if let Some(draws) = value["draws"].as_array() {
                for draw in draws {
                    rows.push(Row {
                        lane: "GPU",
                        key: format!("draw {}", draw["draw"]),
                        value: format!(
                            "command {} · {} vertices · inspect frozen state",
                            draw["command"], draw["vertices"]
                        ),
                        action: draw["draw"].as_u64().map(|draw| D::GpuDraw { draw }),
                    });
                }
            }
        } else {
            if let Some(resources) = value["capture"]["resources"].as_array() {
                for resource in resources {
                    if let (Some(address), Some(width), Some(height), Some(format)) = (
                        resource["address"].as_u64(),
                        resource["width"].as_u64(),
                        resource["height"].as_u64(),
                        resource["format"].as_u64(),
                    ) {
                        rows.push(Row {
                            lane: "texture",
                            key: format!("0x{address:08x}"),
                            value: format!("{width}x{height} format {format} · view surface"),
                            action: Some(D::Surface {
                                address: address as u32,
                                width: width as u32,
                                height: height as u32,
                                format: format as u32,
                            }),
                        });
                    }
                }
                if let Some(draw) = value["capture"]["draw"].as_u64() {
                    rows.push(Row {
                        lane: "shader",
                        key: format!("draw {draw}"),
                        value: "Trace vertex 0 from frozen inputs".into(),
                        action: Some(D::ShaderTrace { draw, vertex: 0 }),
                    });
                }
            }
            flatten(value, String::new(), &mut rows, &mut budget, 0);
        }
        Self {
            rows: Arc::new(rows),
            source: format!(
                "{} · pin {} · profile {} · epoch {} · paused {} · cache flush {}",
                value["identity"]["backend"],
                value["identity"]["source"],
                value["profile_hash"],
                value["epoch"],
                value["paused"],
                value["rasterizer_cache_flush"]
            ),
            next: value["next"].as_u64().and_then(|n| n.try_into().ok()),
            lost: value["lost_total"].as_u64().unwrap_or(0),
        }
    }
    pub fn draw_diff(before: &Value, after: &Value) -> Vec<String> {
        let mut changes = Vec::new();
        for key in [
            "registers",
            "uniforms_f32_bits",
            "bool_uniforms",
            "vertex_program",
            "swizzle",
            "resources",
        ] {
            if let (Some(a), Some(b)) = (before[key].as_array(), after[key].as_array()) {
                for i in 0..a.len().max(b.len()) {
                    let same = if key == "resources" {
                        a.get(i).zip(b.get(i)).is_some_and(|(a, b)| {
                            [
                                "address",
                                "bytes",
                                "kind",
                                "content_xxh3_64",
                                "format",
                                "width",
                                "height",
                                "stride",
                            ]
                            .iter()
                            .all(|key| a[*key] == b[*key])
                        })
                    } else {
                        a.get(i) == b.get(i)
                    };
                    if !same {
                        changes.push(format!(
                            "{key}[{i}]: {} -> {}",
                            clipped(&a.get(i).unwrap_or(&Value::Null).to_string(), 256),
                            clipped(&b.get(i).unwrap_or(&Value::Null).to_string(), 256)
                        ));
                        if changes.len() == 128 {
                            return changes;
                        }
                    }
                }
            }
        }
        changes
    }
    fn event(event: &Value) -> Row {
        Row {
            lane: lane(event),
            key: format!("#{} · tick {}", event["sequence"], event["guest_ticks"]),
            value: clipped(&event.to_string(), 1024),
            action: None,
        }
    }
    pub fn load_trace(path: &Path) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        if file.metadata()?.len() > 64 << 20 {
            return Err(io::Error::other("trace exceeds 64 MiB"));
        }
        Self::parse_trace(std::io::BufReader::new(file))
    }
    fn parse_trace(reader: impl io::BufRead) -> io::Result<Self> {
        let mut rows = Vec::new();
        let mut bytes = 0;
        let mut lost = 0;
        for line in reader.lines() {
            let line = line?;
            if line.len() > 65536 {
                return Err(io::Error::other("trace row exceeds 64 KiB"));
            }
            if line.trim().is_empty() {
                continue;
            }
            let event: Value = serde_json::from_str(&line).map_err(io::Error::other)?;
            let event = event.get("data").unwrap_or(&event);
            let row = Self::event(event);
            bytes += row.key.len() + row.value.len();
            if rows.len() == ROW_CAP || bytes > TEXT_BUDGET {
                lost += 1;
                continue;
            }
            rows.push(row);
        }
        Ok(Self {
            source: "Imported JSONL · guest ticks kept separate from host timestamps".into(),
            rows: Arc::new(rows),
            next: None,
            lost,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn large_trace_is_bounded_and_preserves_lanes() {
        let line = b"{\"kind\":\"ipc\",\"guest_ticks\":123,\"sequence\":4}\n";
        let mut data = Vec::new();
        for _ in 0..100001 {
            data.extend_from_slice(line);
        }
        let trace = Inspection::parse_trace(io::Cursor::new(data)).unwrap();
        assert_eq!(trace.rows.len(), 100000);
        assert_eq!(trace.lost, 1);
        assert_eq!(trace.rows[0].lane, "kernel");
    }
    #[test]
    fn settings_only_offer_typed_boolean_edits() {
        let view = Inspection::from_value(
            &serde_json::json!({"settings":[{"name":"use_cpu_jit","value":true}]}),
            Panel::Settings,
        );
        assert!(matches!(view.rows[0].action, Some(D::SettingsSet { .. })));
    }
    #[test]
    fn draw_diff_uses_content_and_registers_instead_of_capture_versions() {
        let a = serde_json::json!({"registers":[1,2],"resources":[{"address":16,"content_xxh3_64":5,"version":1}]});
        let b = serde_json::json!({"registers":[1,3],"resources":[{"address":16,"content_xxh3_64":5,"version":2}]});
        let changes = Inspection::draw_diff(&a, &b);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].starts_with("registers[1]"));
    }
}
