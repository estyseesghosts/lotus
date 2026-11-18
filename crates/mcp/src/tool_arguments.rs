use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where a setting is set (`lotus_settings::Scope`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingsScope {
    /// Host configuration and the desktop window.
    Global,
    /// A card's primary session.
    Primary,
    /// A running secondary session.
    SecondaryRuntime,
}

impl From<SettingsScope> for lotus_settings::Scope {
    fn from(scope: SettingsScope) -> Self {
        match scope {
            SettingsScope::Global => Self::Global,
            SettingsScope::Primary => Self::Primary,
            SettingsScope::SecondaryRuntime => Self::SecondaryRuntime,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsSchemaArgs {
    /// Only settings settable at this scope; omitted lists every setting.
    #[serde(default)]
    pub scope: Option<SettingsScope>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateScenarioArgs {
    /// Host-approved scenario handle; never a filesystem path.
    pub path: String,
}

/// Persistence of a new session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KindArg {
    /// Mounts a persistent card; guest SD/NAND writes are kept.
    Primary,
    /// Disposable: empty, or a copy of `card`; writes are discarded on stop.
    #[default]
    Secondary,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    /// Defaults to `secondary`.
    #[serde(default)]
    pub kind: KindArg,
    /// Primary: card to mount (created if new). Secondary: card to copy, or omit for empty.
    #[serde(default)]
    pub card: Option<String>,
    /// Approved content handle to load paused; omit to create an idle session.
    #[serde(default)]
    pub asset: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionArgs {
    pub session: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoadArgs {
    pub session: String,
    /// Approved content handle from `lotus_status`.
    pub asset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdvanceArgs {
    pub session: String,
    pub frames: u16,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlOperation {
    /// Run continuously until paused; returns immediately.
    Run,
    Pause,
    Reset,
    /// End the session. A secondary's guest data is deleted.
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ControlArgs {
    pub session: String,
    pub operation: ControlOperation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemoryArgs {
    pub session: String,
    pub address: u32,
    pub length: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistersArgs {
    pub session: String,
    /// CPU core, 0 by default; checked against the active core count.
    #[serde(default)]
    pub core: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputArgs {
    pub session: String,
    pub owner: String,
    pub buttons: u32,
    pub x: u16,
    pub y: u16,
    pub touch: bool,
    /// Circle Pad `[x, y]`, each -1..=1, x right, y up. Omitted means centred.
    #[serde(default)]
    pub circle_pad: [f32; 2],
    /// C-stick `[x, y]` (New 3DS), same convention. Omitted means centred.
    #[serde(default)]
    pub c_stick: [f32; 2],
}

/// Typed debug operations, with limits checked by the shared session API.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DebugArgs {
    pub session: String,
    pub command: lotus_protocol::debug::DebugCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GdbOperation {
    Start,
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GdbArgs {
    pub session: String,
    pub operation: GdbOperation,
    #[serde(default)]
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReleaseArtifactsArgs {
    /// `lotus://artifact/...` URIs; all must be catalogued or none is released.
    pub uris: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PinArtifactArgs {
    pub uri: String,
    /// True keeps the artifact until released; false makes it evictable again.
    pub pinned: bool,
}
