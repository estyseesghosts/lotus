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

/// Saved global defaults, a primary card's saved overrides, or a running session.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum SettingsTargetArg {
    Global,
    Card { card: String },
    Session { session: String },
}

impl From<SettingsTargetArg> for lotus_session::supervisor::SettingsTarget {
    fn from(target: SettingsTargetArg) -> Self {
        match target {
            SettingsTargetArg::Global => Self::Global,
            SettingsTargetArg::Card { card } => Self::Card { card },
            SettingsTargetArg::Session { session } => Self::Session { session },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsArgs {
    pub target: SettingsTargetArg,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingsOperation {
    /// Save (global, card) or apply now (secondary session) `key` = `value`.
    Set,
    /// Remove the override of `key`, resuming inheritance.
    Unset,
    /// Remove every override of the target.
    Reset,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsChangeArgs {
    pub target: SettingsTargetArg,
    pub operation: SettingsOperation,
    /// Registry key, e.g. `cpu_clock_percentage`; required for set and unset.
    #[serde(default)]
    pub key: Option<String>,
    /// JSON value checked against the registry type and bounds; required for set.
    #[serde(default)]
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InputPresetOperation {
    /// Builtin and saved presets, with files that could not be read.
    List,
    /// One preset's JSON (`name`).
    Export,
    /// Save exported JSON (`json`); a taken name gets a `-N` suffix.
    Import,
    /// Copy `name` (builtin or saved) to `to`.
    Duplicate,
    /// Rename saved preset `name` to `to`.
    Rename,
    /// Delete saved preset `name`.
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputPresetArgs {
    pub operation: InputPresetOperation,
    #[serde(default)]
    pub name: Option<String>,
    /// New name for duplicate and rename: 1-32 of a-z, 0-9, `-`, `_`.
    #[serde(default)]
    pub to: Option<String>,
    /// Exported preset for import.
    #[serde(default)]
    pub json: Option<String>,
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
pub struct CaptureImageArgs {
    pub session: String,
    /// Integer scale from 1 to 4. Omit for native screen dimensions.
    #[schemars(range(min = 1, max = 4))]
    pub scale: Option<u32>,
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

/// `lotus_workspace` (lotus#72): read or change what the LOTUS window shows. Mirrors
/// `lotus_session::workspace::Operation`, which has no JSON schema of its own.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceArgs {
    pub operation: WorkspaceOperation,
    /// Refuse the change unless the workspace is still at this revision (from `get` or
    /// `lotus_status.workspace`), so a newer change by the person is never overwritten.
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AxisArg {
    Auto,
    /// First tile left, second right.
    Horizontal,
    /// First tile on top, second below.
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScreensArg {
    Auto,
    Stacked,
    SideBySide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ViewportArg {
    First,
    Second,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceOperation {
    /// Current revision, layout, focused viewport and split defaults. Read-only.
    Get,
    /// Show two different sessions together. Unset fields keep the current split's value,
    /// or the saved defaults. Ratio is the first tile's share, 0.2 to 0.8.
    Tile {
        first: String,
        second: String,
        #[serde(default)]
        axis: Option<AxisArg>,
        #[serde(default)]
        ratio: Option<f32>,
        #[serde(default)]
        screens: Option<[ScreensArg; 2]>,
    },
    /// Show one session alone; the other tile's session returns to the sidebar.
    Single {
        session: String,
    },
    /// Leave a split, keeping `keep` (default: the first tile).
    Untile {
        #[serde(default)]
        keep: Option<String>,
    },
    Swap,
    SetAxis {
        axis: AxisArg,
    },
    SetRatio {
        ratio: f32,
    },
    SetScreenLayout {
        viewport: ViewportArg,
        screens: ScreensArg,
    },
    /// Which tile receives the window's keyboard input.
    FocusViewport {
        viewport: ViewportArg,
    },
}

impl From<AxisArg> for lotus_session::workspace::Axis {
    fn from(axis: AxisArg) -> Self {
        match axis {
            AxisArg::Auto => Self::Auto,
            AxisArg::Horizontal => Self::Horizontal,
            AxisArg::Vertical => Self::Vertical,
        }
    }
}

impl From<ScreensArg> for lotus_session::workspace::ScreenLayout {
    fn from(screens: ScreensArg) -> Self {
        match screens {
            ScreensArg::Auto => Self::Auto,
            ScreensArg::Stacked => Self::Stacked,
            ScreensArg::SideBySide => Self::SideBySide,
        }
    }
}

impl From<ViewportArg> for lotus_session::workspace::Viewport {
    fn from(viewport: ViewportArg) -> Self {
        match viewport {
            ViewportArg::First => Self::First,
            ViewportArg::Second => Self::Second,
        }
    }
}

impl WorkspaceOperation {
    /// The workspace change, or `None` for `get`.
    #[must_use]
    pub fn change(self) -> Option<lotus_session::workspace::Operation> {
        use lotus_session::workspace::Operation;
        Some(match self {
            Self::Get => return None,
            Self::Tile {
                first,
                second,
                axis,
                ratio,
                screens,
            } => Operation::Tile {
                first,
                second,
                axis: axis.map(Into::into),
                ratio,
                screens: screens.map(|pair| pair.map(Into::into)),
            },
            Self::Single { session } => Operation::Single { session },
            Self::Untile { keep } => Operation::Untile { keep },
            Self::Swap => Operation::Swap,
            Self::SetAxis { axis } => Operation::SetAxis { axis: axis.into() },
            Self::SetRatio { ratio } => Operation::SetRatio { ratio },
            Self::SetScreenLayout { viewport, screens } => Operation::SetScreenLayout {
                viewport: viewport.into(),
                screens: screens.into(),
            },
            Self::FocusViewport { viewport } => Operation::FocusViewport {
                viewport: viewport.into(),
            },
        })
    }
}
