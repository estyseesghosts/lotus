//! One typed registry of settings (lotus#50): Azahar worker settings and Lotus-owned ones.
//!
//! The Azahar half is generated: `azahar-inventory.json` is written by
//! `tools/settings_inventory.py` from the pinned workers' `settings_list`
//! (`ctr_worker/debug.h`). It holds each binding's type, stock default, range, choices, apply
//! class per branch and consumer citations. This crate adds what the worker cannot know:
//! intent group, label, scopes, owner and availability, and the Lotus-owned settings of the
//! host configuration and the desktop. CLI, MCP and the desktop read descriptors from here
//! instead of keeping their own bounds and defaults.
//!
//! Keys: Azahar settings keep their worker names (`graphics_api`, `cfg.000a0000`), as host
//! profiles already use them. Lotus settings are `lotus.host.*`, `lotus.desktop.*` and the
//! per-session input layout `lotus.input.*` (lotus#63).
//! One-shot actions (capture a GPU frame, start a trace, set a breakpoint, create a session)
//! are tools, not settings, and are not registered.

mod azahar;
mod lotus;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::Serialize;
use serde_json::Value;

/// Settings groups by what a person wants to change, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    General,
    SessionsStorage,
    Emulation,
    GraphicsDisplay,
    AudioDevices,
    Controls,
    DebuggingTracing,
    McpAutomation,
    Discord,
    Advanced,
}

impl Group {
    pub const ALL: [Group; 10] = [
        Group::General,
        Group::SessionsStorage,
        Group::Emulation,
        Group::GraphicsDisplay,
        Group::AudioDevices,
        Group::Controls,
        Group::DebuggingTracing,
        Group::McpAutomation,
        Group::Discord,
        Group::Advanced,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Group::General => "General",
            Group::SessionsStorage => "Sessions & Storage",
            Group::Emulation => "Emulation",
            Group::GraphicsDisplay => "Graphics & Display",
            Group::AudioDevices => "Audio & Devices",
            Group::Controls => "Controls",
            Group::DebuggingTracing => "Debugging & Tracing",
            Group::McpAutomation => "MCP & Automation",
            Group::Discord => "Discord",
            Group::Advanced => "Advanced",
        }
    }
}

/// Who reads the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    /// The emulator worker, through `settings_set`.
    Azahar,
    /// The supervisor's host configuration; used by the desktop, `lotus-mcp` and `ctr-cli`.
    Host,
    /// The desktop window only (`desktop.json`); headless frontends never read it.
    Desktop,
    /// A session's input layout (lotus#63). Layered like worker settings (global, card,
    /// secondary runtime) but applied by the desktop's input router, never sent to a worker.
    Input,
}

/// When a new value takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Apply {
    /// Azahar: applied on a running session (`System::ApplySettings` or read per request).
    Live,
    /// Azahar: fixed when the session's renderer is created.
    Renderer,
    /// Azahar: read while the system or a service initializes.
    Boot,
    /// Azahar: written to the console's CFG savedata block.
    CfgSavedata,
    /// Azahar: no consumer in core, video_core or audio_core.
    Inert,
    /// Lotus host: read when the host starts.
    HostStart,
    /// Lotus host: read when each session is created.
    SessionCreate,
    /// Desktop: applied by the window at once.
    Immediate,
    /// Input layout: applied to the session's input routing when it changes, while running.
    Routing,
}

impl Apply {
    fn parse(class: &str) -> Option<Self> {
        Some(match class {
            "live" => Apply::Live,
            "renderer" => Apply::Renderer,
            "boot" => Apply::Boot,
            "cfg-savedata" => Apply::CfgSavedata,
            "inert" => Apply::Inert,
            _ => return None,
        })
    }
}

/// Where a setting may be set (resolution layers, lotus#51).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct Scopes {
    pub global: bool,
    pub primary: bool,
    pub secondary_runtime: bool,
}

impl Scopes {
    pub const GLOBAL: Scopes = Scopes {
        global: true,
        primary: false,
        secondary_runtime: false,
    };
    pub const ALL: Scopes = Scopes {
        global: true,
        primary: true,
        secondary_runtime: true,
    };
    pub const NONE: Scopes = Scopes {
        global: false,
        primary: false,
        secondary_runtime: false,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Global,
    Primary,
    SecondaryRuntime,
}

impl Scopes {
    #[must_use]
    pub fn allows(self, scope: Scope) -> bool {
        match scope {
            Scope::Global => self.global,
            Scope::Primary => self.primary,
            Scope::SecondaryRuntime => self.secondary_runtime,
        }
    }
}

/// Value type and bounds.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    Boolean,
    Integer {
        min: Option<i64>,
        max: Option<i64>,
        /// Allowed values when the setting is an enum.
        choices: Option<Vec<i64>>,
        nullable: bool,
    },
    Float {
        min: Option<f64>,
        max: Option<f64>,
    },
    String {
        max_len: usize,
        /// Allowed values when the setting is a named choice.
        choices: Option<Vec<String>>,
        nullable: bool,
        /// Exact length for fixed-size hex blobs (CFG blocks).
        exact_len: Option<usize>,
        hex: bool,
    },
    /// Structured Azahar values (arrays, objects); validated by the worker.
    Json {
        max_bytes: usize,
    },
    /// One input action's binding list (`lotus_input::layout::Binding`), or null to keep the
    /// preset's.
    Bindings {
        max: usize,
    },
}

/// Worker build a setting exists in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Branch {
    /// AZAHAR worker (`integration` branch).
    Integration,
    /// CITnLO worker (`experimental` branch).
    Experimental,
}

impl Branch {
    /// From a worker's `hello.flavor`.
    #[must_use]
    pub fn from_flavor(flavor: &str) -> Option<Self> {
        match flavor {
            "AZAHAR" => Some(Branch::Integration),
            "CITnLO" => Some(Branch::Experimental),
            _ => None,
        }
    }
}

/// Whether a person can use the setting, and why not.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    Available,
    /// Present in one worker build only.
    BranchOnly {
        branch: Branch,
    },
    /// Never offered as a control; `reason` says why, with a citation.
    Excluded {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Descriptor {
    pub key: String,
    pub owner: Owner,
    pub group: Group,
    pub subgroup: &'static str,
    pub label: String,
    pub description: String,
    pub kind: Kind,
    pub default: Value,
    pub scopes: Scopes,
    /// Apply class; Azahar settings carry one per branch (they agree for every key today).
    pub apply: Apply,
    pub availability: Availability,
    /// Consumer citations (Azahar) or the Rust item that reads the value (Lotus).
    pub sources: Vec<String>,
    pub keywords: Vec<&'static str>,
}

impl Descriptor {
    /// Offered as a control on a worker of `branch` (Lotus settings ignore the branch).
    pub fn available_on(&self, branch: Option<Branch>) -> Result<(), String> {
        match &self.availability {
            Availability::Available => Ok(()),
            Availability::BranchOnly { branch: only } => match branch {
                Some(b) if b == *only => Ok(()),
                _ => Err(format!(
                    "{} exists only in the {} worker",
                    self.key,
                    match only {
                        Branch::Integration => "AZAHAR",
                        Branch::Experimental => "CITnLO",
                    }
                )),
            },
            Availability::Excluded { reason } => Err(format!("{}: {reason}", self.key)),
        }
    }

    /// Check `value` against the type and bounds; returns it unchanged when valid.
    pub fn validate(&self, value: &Value) -> Result<(), SettingError> {
        let invalid = |reason: String| SettingError::Invalid {
            key: self.key.clone(),
            reason,
        };
        match &self.kind {
            Kind::Boolean => {
                if !value.is_boolean() {
                    return Err(invalid("expected true or false".into()));
                }
            }
            Kind::Integer {
                min,
                max,
                choices,
                nullable,
            } => {
                if value.is_null() && *nullable {
                    return Ok(());
                }
                let Some(number) = value.as_i64() else {
                    return Err(invalid("expected an integer".into()));
                };
                if min.is_some_and(|min| number < min) || max.is_some_and(|max| number > max) {
                    return Err(invalid(format!(
                        "{number} outside {}..={}",
                        min.map_or("".into(), |m| m.to_string()),
                        max.map_or("".into(), |m| m.to_string())
                    )));
                }
                if let Some(choices) = choices {
                    if !choices.contains(&number) {
                        return Err(invalid(format!("{number} is not one of {choices:?}")));
                    }
                }
            }
            Kind::Float { min, max } => {
                let Some(number) = value.as_f64().filter(|n| n.is_finite()) else {
                    return Err(invalid("expected a finite number".into()));
                };
                if min.is_some_and(|min| number < min) || max.is_some_and(|max| number > max) {
                    return Err(invalid(format!("{number} out of range")));
                }
            }
            Kind::String {
                max_len,
                choices,
                nullable,
                exact_len,
                hex,
            } => {
                if value.is_null() && *nullable {
                    return Ok(());
                }
                let Some(text) = value.as_str() else {
                    return Err(invalid("expected a string".into()));
                };
                if text.len() > *max_len {
                    return Err(invalid(format!("longer than {max_len} bytes")));
                }
                if exact_len.is_some_and(|len| text.len() != len) {
                    return Err(invalid(format!(
                        "expected exactly {} characters",
                        exact_len.unwrap_or_default()
                    )));
                }
                if *hex
                    && !text
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err(invalid("expected lowercase hex".into()));
                }
                if let Some(choices) = choices {
                    if !choices.iter().any(|c| c == text) {
                        return Err(invalid(format!("{text} is not one of {choices:?}")));
                    }
                }
            }
            Kind::Json { max_bytes } => {
                if value.to_string().len() > *max_bytes {
                    return Err(invalid(format!("larger than {max_bytes} bytes")));
                }
            }
            Kind::Bindings { max } => {
                if value.is_null() {
                    return Ok(());
                }
                let Some(list) = value.as_array() else {
                    return Err(invalid("expected a list of bindings or null".into()));
                };
                if list.len() > *max {
                    return Err(invalid(format!("more than {max} bindings")));
                }
                // Any action parses the same bindings; the key names the action.
                let problems =
                    lotus_input::layout::parse_list(lotus_input::layout::Action::A, value)
                        .map_or_else(|p| vec![p], |(_, problems)| problems);
                if let Some((key, reason)) = problems.iter().find_map(|p| match p {
                    lotus_input::layout::Problem::ReservedKey { key, reason, .. } => {
                        Some((key, reason))
                    }
                    _ => None,
                }) {
                    return Err(invalid(format!(
                        "key '{key}' is reserved by the desktop ({reason})"
                    )));
                }
                if !problems.is_empty() {
                    return Err(invalid(format!(
                        "{} binding(s) this build cannot read",
                        problems.len()
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingError {
    Unknown { key: String },
    Invalid { key: String, reason: String },
    Unavailable { reason: String },
    Scope { key: String, scope: Scope },
}

impl std::fmt::Display for SettingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SettingError::Unknown { key } => write!(f, "unknown setting {key}"),
            SettingError::Invalid { key, reason } => write!(f, "invalid value for {key}: {reason}"),
            SettingError::Unavailable { reason } => write!(f, "{reason}"),
            SettingError::Scope { key, scope } => {
                write!(f, "{key} cannot be set at {scope:?} scope")
            }
        }
    }
}

impl std::error::Error for SettingError {}

/// Keys a worker reports that the registry lacks, and registry keys it lacks.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Drift {
    pub unknown_to_registry: Vec<String>,
    pub missing_from_worker: Vec<String>,
}

impl Drift {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.unknown_to_registry.is_empty() && self.missing_from_worker.is_empty()
    }
}

/// Descriptor inventory with lookup by key.
#[derive(Debug)]
pub struct Registry {
    descriptors: Vec<Descriptor>,
    index: BTreeMap<String, usize>,
    /// Worker identity the Azahar half was generated from (`azahar-inventory.json`).
    pub generated_from: Value,
}

impl Registry {
    /// The registry built into this binary.
    #[must_use]
    pub fn builtin() -> &'static Registry {
        static REGISTRY: OnceLock<Registry> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            let (mut descriptors, generated_from) = azahar::descriptors();
            descriptors.extend(lotus::descriptors());
            let index = descriptors
                .iter()
                .enumerate()
                .map(|(i, d)| (d.key.clone(), i))
                .collect();
            Registry {
                descriptors,
                index,
                generated_from,
            }
        })
    }

    #[must_use]
    pub fn descriptors(&self) -> &[Descriptor] {
        &self.descriptors
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Descriptor> {
        self.index.get(key).map(|&i| &self.descriptors[i])
    }

    /// Descriptors settable at `scope`, in group order.
    pub fn for_scope(&self, scope: Scope) -> impl Iterator<Item = &Descriptor> {
        self.descriptors
            .iter()
            .filter(move |d| d.scopes.allows(scope))
    }

    /// Validate a value for `key` at `scope` on a worker of `branch` (`None`: branch unknown,
    /// so branch-only Azahar settings are refused).
    pub fn validate(
        &self,
        key: &str,
        value: &Value,
        scope: Scope,
        branch: Option<Branch>,
    ) -> Result<&Descriptor, SettingError> {
        let descriptor = self.get(key).ok_or_else(|| SettingError::Unknown {
            key: key.to_string(),
        })?;
        descriptor
            .available_on(branch)
            .map_err(|reason| SettingError::Unavailable { reason })?;
        if !descriptor.scopes.allows(scope) {
            return Err(SettingError::Scope {
                key: key.to_string(),
                scope,
            });
        }
        descriptor.validate(value)?;
        Ok(descriptor)
    }

    /// Compare the names a worker's `settings_list` reported with the Azahar descriptors for
    /// its branch.
    #[must_use]
    pub fn drift<'a>(&self, branch: Branch, worker: impl IntoIterator<Item = &'a str>) -> Drift {
        let reported: BTreeSet<&str> = worker.into_iter().collect();
        let expected: BTreeSet<&str> = self
            .descriptors
            .iter()
            .filter(|d| d.owner == Owner::Azahar)
            .filter(|d| match d.availability {
                Availability::BranchOnly { branch: only } => only == branch,
                _ => true,
            })
            .map(|d| d.key.as_str())
            .collect();
        Drift {
            unknown_to_registry: reported
                .difference(&expected)
                .map(|s| s.to_string())
                .collect(),
            missing_from_worker: expected
                .difference(&reported)
                .map(|s| s.to_string())
                .collect(),
        }
    }

    /// Schema for discovery (CLI, MCP, UI): every descriptor, grouped.
    #[must_use]
    pub fn schema(&self, scope: Option<Scope>) -> Value {
        let groups: Vec<Value> = Group::ALL
            .iter()
            .map(|group| {
                let settings: Vec<&Descriptor> = self
                    .descriptors
                    .iter()
                    .filter(|d| d.group == *group)
                    .filter(|d| scope.is_none_or(|s| d.scopes.allows(s)))
                    .collect();
                serde_json::json!({"group": group, "label": group.label(), "settings": settings})
            })
            .collect();
        serde_json::json!({
            "version": 1,
            "generated_from": self.generated_from,
            "groups": groups,
        })
    }
}

#[cfg(test)]
mod tests;
