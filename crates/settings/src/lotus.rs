//! Lotus-owned settings: host configuration (`HostConfig`, read by every frontend), desktop
//! preferences (`desktop.json`, the window only), session input layouts (lotus#63) and
//! debugger request defaults (lotus#56). Defaults and bounds here are checked against
//! the owning code by tests in `lotus-session` and `lotus-desktop`, so they cannot drift apart.

use serde_json::{json, Value};

use crate::{Apply, Availability, Descriptor, Group, Kind, Owner, Scopes, SETTINGS_BOUNDS_KEY};

#[allow(clippy::too_many_arguments)]
fn setting(
    key: &str,
    owner: Owner,
    group: Group,
    subgroup: &'static str,
    label: &str,
    description: &str,
    kind: Kind,
    default: Value,
    apply: Apply,
    source: &str,
    keywords: &[&'static str],
) -> Descriptor {
    Descriptor {
        key: key.to_string(),
        owner,
        group,
        subgroup,
        label: label.to_string(),
        description: description.to_string(),
        kind,
        default,
        // Host and desktop settings have one value per host or window, never per card.
        scopes: Scopes::GLOBAL,
        apply,
        availability: Availability::Available,
        sources: vec![source.to_string()],
        keywords: keywords.to_vec(),
    }
}

fn integer(min: i64, max: i64) -> Kind {
    Kind::Integer {
        min: Some(min),
        max: Some(max),
        choices: None,
        nullable: false,
    }
}

/// Unsigned host values: the code checks only the lower bound (`ArtifactQuota` refuses 0
/// count or bytes, `artifacts.rs`), so no upper bound is invented here.
fn at_least(min: i64) -> Kind {
    Kind::Integer {
        min: Some(min),
        max: None,
        choices: None,
        nullable: false,
    }
}

fn optional_budget() -> Kind {
    Kind::Integer {
        min: Some(0),
        max: None,
        choices: None,
        nullable: true,
    }
}

fn choice(choices: &[&str]) -> Kind {
    Kind::String {
        max_len: 32,
        choices: Some(choices.iter().map(|c| c.to_string()).collect()),
        nullable: false,
        exact_len: None,
        hex: false,
    }
}

/// Bindings an action may hold; the editor offers a few per device, so this only bounds
/// stored values.
const MAX_BINDINGS: usize = 8;

fn float(min: f64, max: f64) -> Kind {
    Kind::Float {
        min: Some(min),
        max: Some(max),
    }
}

/// `lotus.input.*` (lotus#63): layered at every scope like worker settings, read by
/// `lotus_input::layout` through `lotus_session::settings_store`. Defaults are the builtin
/// `default` preset with no overrides and Azahar's stick and trigger defaults.
fn input_descriptors() -> Vec<Descriptor> {
    use lotus_input::layout::Action;
    let source = "lotus_input::layout";
    let input = |key: String,
                 subgroup: &'static str,
                 label: String,
                 description: &str,
                 kind: Kind,
                 default: Value,
                 keywords: &[&'static str]| Descriptor {
        key,
        owner: Owner::Input,
        group: Group::Controls,
        subgroup,
        label,
        description: description.to_string(),
        kind,
        default,
        scopes: Scopes::ALL,
        apply: Apply::Routing,
        availability: Availability::Available,
        sources: vec![source.to_string()],
        keywords: keywords.to_vec(),
    };
    let mut list = vec![input(
        "lotus.input.preset".into(),
        "Layout",
        "Input preset".into(),
        "Named layout the bindings start from: a builtin (default, xbox-labels) or a saved \
         preset. A missing preset falls back to default.",
        Kind::String {
            max_len: 32,
            choices: None,
            nullable: true,
            exact_len: None,
            hex: false,
        },
        Value::Null,
        &["preset", "layout", "mapping"],
    )];
    for action in Action::ALL {
        let id = action.id();
        list.push(input(
            format!("lotus.input.bind.{id}"),
            "Bindings",
            format!("Bind {id}"),
            "Replaces the preset's bindings for this 3DS input; an empty list unbinds it, \
             null keeps the preset's.",
            Kind::Bindings { max: MAX_BINDINGS },
            Value::Null,
            &["binding", "button", "key", "mapping"],
        ));
    }
    for (stick, name) in [("circle_pad", "Circle Pad"), ("c_stick", "C-stick")] {
        list.push(input(
            format!("lotus.input.{stick}.deadzone"),
            "Sticks",
            format!("{name} dead zone"),
            "Radial dead zone with Azahar's rescaling, on top of the controller driver's; \
             Azahar's range is 0 to 0.99 and its default 0 (sdl_impl.cpp).",
            float(0.0, 0.99),
            json!(0.0),
            &["deadzone", "stick", "drift"],
        ));
        list.push(input(
            format!("lotus.input.{stick}.sensitivity"),
            "Sticks",
            format!("{name} sensitivity"),
            "Scale after the dead zone, clamped to full deflection. At 2, half travel \
             reaches full deflection.",
            float(0.1, 2.0),
            json!(1.0),
            &["sensitivity", "stick"],
        ));
        list.push(input(
            format!("lotus.input.{stick}.invert_y"),
            "Sticks",
            format!("Invert {name} Y"),
            "Flip the vertical axis.",
            Kind::Boolean,
            json!(false),
            &["invert", "stick"],
        ));
    }
    list.push(input(
        "lotus.input.threshold".into(),
        "Sticks",
        "Axis press threshold".into(),
        "Travel at which an axis bound to a button presses it (triggers, stick-to-D-pad). \
         Azahar uses 0.5 for ZL/ZR; its range is open (0, 1) (sdl_impl.cpp).",
        float(0.01, 0.99),
        json!(0.5),
        &["trigger", "threshold", "axis"],
    ));
    list
}

/// `lotus.debug.*` (lotus#56): global and card defaults the supervisor fills into a
/// `rewind_configure` or `gpu_configure` request that omits the field, for every frontend
/// (`lotus_session::supervisor::Supervisor::debug_defaults`). Unset leaves the field out, so
/// the worker keeps its own default. Bounds are `DebugCommand::validate`'s
/// (`lotus-protocol/src/debug.rs`); worker defaults are cited from the field docs there.
fn debug_descriptors() -> Vec<Descriptor> {
    let source = "lotus_session::supervisor::Supervisor::debug_defaults";
    let debug = |key: &str,
                 subgroup: &'static str,
                 label: &str,
                 description: &str,
                 kind: Kind,
                 keywords: &[&'static str]| Descriptor {
        key: key.to_string(),
        owner: Owner::Debugger,
        group: Group::DebuggingTracing,
        subgroup,
        label: label.to_string(),
        description: description.to_string(),
        kind,
        default: Value::Null,
        scopes: Scopes::SAVED,
        apply: Apply::DebugRequest,
        availability: Availability::Available,
        sources: vec![source.to_string()],
        keywords: keywords.to_vec(),
    };
    let optional = |min: i64, max: i64| Kind::Integer {
        min: Some(min),
        max: Some(max),
        choices: None,
        nullable: true,
    };
    vec![
        debug(
            "lotus.debug.rewind.interval_frames",
            "Rewind Defaults",
            "Checkpoint interval",
            "Frames between rewind checkpoints when enabling rewind does not name one. Larger              values cut savestate cost; reverse steps then re-execute longer windows. Input              changes always force a checkpoint. Unset: the worker default, 1.",
            optional(1, 60),
            &["rewind", "checkpoint", "cadence", "interval"],
        ),
        debug(
            "lotus.debug.rewind.compression_level",
            "Rewind Defaults",
            "Checkpoint compression level",
            "zstd level of stored rewind checkpoints when the request does not name one. Every              level restores the same state; lower levels trade memory for checkpoint time.              Unset: the worker default, 3.",
            optional(-7, 19),
            &["rewind", "zstd", "compression", "memory"],
        ),
        debug(
            "lotus.debug.rewind.compression_queue",
            "Rewind Defaults",
            "Background compression queue",
            "Checkpoints compressed off the emulation thread at once when the request does not              name a count; 0 compresses on the emulation thread. Used only by workers that              report rewind_compression_queue (CITnLO). Unset: the worker default, 1.",
            optional(0, 4),
            &["rewind", "compression", "queue", "thread"],
        ),
        debug(
            "lotus.debug.gpu.capture",
            "GPU Capture Defaults",
            "GPU capture level",
            "Capture level when enabling GPU capture does not name one: counters, events,              draws or full. Used only by workers that report gpu_capture_levels (CITnLO).              Counters accepts no stop events, so a request with stop events keeps the worker              default. Unset: the worker default, full.",
            Kind::String {
                max_len: 8,
                choices: Some(
                    ["counters", "events", "draws", "full"]
                        .iter()
                        .map(|c| c.to_string())
                        .collect(),
                ),
                nullable: true,
                exact_len: None,
                hex: false,
            },
            &["gpu", "pica", "capture", "trace"],
        ),
    ]
}

pub(crate) fn descriptors() -> Vec<Descriptor> {
    use Apply::{HostStart, Immediate, SessionCreate};
    use Group::*;
    use Owner::{Desktop, Host};
    let host = "lotus_session::supervisor::HostConfig";
    let policy = "lotus_session::policy::Policy";
    let quota = "lotus_session::artifacts::ArtifactQuota";
    let prefs = "lotus_desktop::preferences::Preferences";
    vec![
        setting(
            "lotus.host.max_sessions",
            Host,
            SessionsStorage,
            "Sessions",
            "Maximum sessions",
            "Workers this host runs at once.",
            integer(1, 16),
            json!(4),
            HostStart,
            host,
            &["limit", "workers"],
        ),
        setting(
            "lotus.host.storage_root",
            Host,
            SessionsStorage,
            "Storage",
            "Storage root",
            "Guest storage: cards/<card> holds each card's SD, NAND and system data and its              saved overrides; scratch/<session> holds a secondary's copy, deleted when it ends              (logs kept after a crash). Generated files: artifacts/ holds captures under the              artifact quota, removed when the next host opens the store. Host caches:              shader-seed/ holds shader caches copied into new workers. Global defaults are              settings/global.json.",
            Kind::Json { max_bytes: 4096 },
            Value::Null,
            HostStart,
            host,
            &["storage", "folder", "location", "saves", "artifacts", "cache"],
        ),
        setting(
            "lotus.host.assets",
            Host,
            McpAutomation,
            "Approved Content",
            "Approved content files",
            "Content clients may load, by handle. Clients name a handle, never a path.",
            Kind::Json { max_bytes: 65_536 },
            json!({}),
            HostStart,
            host,
            &["assets", "homebrew", "3dsx", "approved", "files"],
        ),
        setting(
            "lotus.host.asset_directories",
            Host,
            McpAutomation,
            "Approved Content",
            "Approved content directories",
            "Every .3dsx directly inside one of these is loadable as <handle>/<file>;              subdirectories are not.",
            Kind::Json { max_bytes: 65_536 },
            json!({}),
            HostStart,
            host,
            &["assets", "directory", "folder", "approved"],
        ),
        setting(
            "lotus.host.source_roots",
            Host,
            DebuggingTracing,
            "Source",
            "Source roots",
            "Local directories source excerpts may read, by handle, with the build-time path              prefix each replaces.",
            Kind::Json { max_bytes: 65_536 },
            json!({}),
            HostStart,
            host,
            &["source", "dwarf", "symbols", "path"],
        ),
        setting(
            "lotus.host.prefer_vulkan",
            Host,
            GraphicsDisplay,
            "Renderer",
            "Prefer Vulkan",
            "New sessions whose profile names no graphics_api use Vulkan, \
             falling back to software when the worker refuses Vulkan setup.",
            Kind::Boolean,
            json!(true),
            SessionCreate,
            host,
            &["vulkan", "renderer", "gpu"],
        ),
        setting(
            "lotus.host.shader_seed",
            Host,
            SessionsStorage,
            "Startup",
            "Seed shader caches",
            "Copy the shader cache of the last clean stop of the same \
             worker binary into each new worker root.",
            Kind::Boolean,
            json!(false),
            SessionCreate,
            host,
            &["shader", "cache", "startup"],
        ),
        setting(
            "lotus.host.allow_control",
            Host,
            McpAutomation,
            "Permissions",
            "Allow control",
            "Allow creating, running and changing sessions. Off makes the \
             host read-only for MCP clients and the window.",
            Kind::Boolean,
            json!(false),
            HostStart,
            host,
            &["read-only", "permissions"],
        ),
        setting(
            "lotus.host.artifacts.max_count",
            Host,
            McpAutomation,
            "Artifacts",
            "Artifact count quota",
            "Captures kept across all sessions.",
            at_least(1),
            json!(512),
            HostStart,
            quota,
            &["captures", "quota"],
        ),
        setting(
            "lotus.host.artifacts.max_bytes",
            Host,
            McpAutomation,
            "Artifacts",
            "Artifact byte quota",
            "Bytes of captures kept across all sessions.",
            at_least(1),
            json!(512_i64 << 20),
            HostStart,
            quota,
            &["captures", "quota"],
        ),
        setting(
            "lotus.host.artifacts.lease_seconds",
            Host,
            McpAutomation,
            "Artifacts",
            "Artifact lease",
            "Seconds a new capture is protected from eviction by other \
             sessions.",
            at_least(0),
            json!(60),
            HostStart,
            quota,
            &["captures", "eviction"],
        ),
        setting(
            "lotus.host.policy.workload",
            Host,
            SessionsStorage,
            "Resource Policy",
            "Workload",
            "interactive budgets expensive debugger work; batch leaves it \
             unlimited unless set.",
            choice(&["interactive", "batch"]),
            json!("interactive"),
            HostStart,
            policy,
            &["budget", "throughput"],
        ),
        setting(
            "lotus.host.policy.rewind_sessions",
            Host,
            SessionsStorage,
            "Resource Policy",
            "Rewind sessions",
            "Sessions recording rewind checkpoints at once. Unset: 2 \
             interactive, unlimited batch; 0 is unlimited.",
            optional_budget(),
            Value::Null,
            HostStart,
            policy,
            &["rewind", "budget"],
        ),
        setting(
            "lotus.host.policy.rewind_memory_mib",
            Host,
            SessionsStorage,
            "Resource Policy",
            "Rewind memory estimate limit",
            "MiB reserved for rewind buffers and history. Unset: 1536 interactive, unlimited batch. 0 is unlimited.",
            optional_budget(),
            Value::Null,
            HostStart,
            policy,
            &["rewind", "memory", "budget"],
        ),
        setting(
            "lotus.host.policy.gpu_capture_sessions",
            Host,
            SessionsStorage,
            "Resource Policy",
            "GPU capture sessions",
            "Sessions with GPU capture above \
             counters at once. Unset: 2 interactive, unlimited batch; 0 is unlimited.",
            optional_budget(),
            Value::Null,
            HostStart,
            policy,
            &["gpu", "capture", "budget"],
        ),
        setting(
            "lotus.host.policy.snapshot_slots",
            Host,
            SessionsStorage,
            "Resource Policy",
            "Snapshot slots",
            "Full-state operations at once across the host. Unset: 2 \
             interactive, unlimited batch; 0 is unlimited.",
            optional_budget(),
            Value::Null,
            HostStart,
            policy,
            &["save state", "fork"],
        ),
        setting(
            "lotus.host.policy.snapshot_queue_limit",
            Host,
            SessionsStorage,
            "Resource Policy",
            "Snapshot queue limit",
            "Maximum requests waiting for a snapshot slot. 0 refuses a wait.",
            at_least(0),
            json!(16),
            HostStart,
            policy,
            &["snapshot", "queue"],
        ),
        setting(
            "lotus.host.policy.snapshot_wait_ms",
            Host,
            SessionsStorage,
            "Resource Policy",
            "Snapshot wait",
            "Longest wait in milliseconds for a snapshot slot.",
            at_least(0),
            json!(10_000),
            HostStart,
            policy,
            &["timeout"],
        ),
        setting(
            "lotus.host.policy.memory_reserve_mib",
            Host,
            SessionsStorage,
            "Resource Policy",
            "Memory reserve",
            "MiB of physical memory kept free when \
             admitting a session; 0 disables the check.",
            at_least(0),
            json!(1024),
            HostStart,
            policy,
            &["ram", "admission"],
        ),
        setting(
            "lotus.host.policy.gpu_admission",
            Host,
            SessionsStorage,
            "Resource Policy",
            "GPU admission",
            "Admit new Vulkan sessions on measured device-memory headroom.",
            Kind::Boolean,
            json!(true),
            HostStart,
            policy,
            &["vram", "vulkan"],
        ),
        setting(
            "lotus.host.policy.gpu_reserve_mib",
            Host,
            SessionsStorage,
            "Resource Policy",
            "GPU reserve",
            "MiB of device-local memory kept free beyond a session's \
             estimated need.",
            at_least(0),
            json!(0),
            HostStart,
            policy,
            &["vram"],
        ),
        setting(
            "lotus.host.policy.focus_priority",
            Host,
            SessionsStorage,
            "Resource Policy",
            "Focus priority",
            "Run the focused session's worker in the above-normal priority \
             class (Windows).",
            Kind::Boolean,
            json!(false),
            HostStart,
            policy,
            &["priority", "focus"],
        ),
        setting(
            "lotus.desktop.sessions_open",
            Desktop,
            General,
            "Window Layout",
            "Show sessions sidebar",
            "Sidebar open at start.",
            Kind::Boolean,
            json!(true),
            Immediate,
            prefs,
            &["sidebar"],
        ),
        setting(
            "lotus.desktop.sessions_width",
            Desktop,
            General,
            "Window Layout",
            "Sessions sidebar width",
            "Width in logical pixels.",
            Kind::Float {
                min: Some(200.0),
                max: Some(440.0),
            },
            json!(264.0),
            Immediate,
            prefs,
            &["sidebar"],
        ),
        setting(
            "lotus.desktop.inspector_open",
            Desktop,
            General,
            "Window Layout",
            "Show inspector",
            "Inspector open at start.",
            Kind::Boolean,
            json!(false),
            Immediate,
            prefs,
            &["inspector"],
        ),
        setting(
            "lotus.desktop.inspector_width",
            Desktop,
            General,
            "Window Layout",
            "Inspector width",
            "Width in logical pixels.",
            Kind::Float {
                min: Some(300.0),
                max: Some(760.0),
            },
            json!(420.0),
            Immediate,
            prefs,
            &["inspector"],
        ),
        setting(
            "lotus.desktop.split_axis",
            Desktop,
            General,
            "Window Layout",
            "Split axis",
            "Default axis for two tiled sessions; auto picks from the window \
             shape.",
            choice(&["auto", "horizontal", "vertical"]),
            json!("auto"),
            Immediate,
            prefs,
            &["split", "tile"],
        ),
        setting(
            "lotus.desktop.split_ratio",
            Desktop,
            General,
            "Window Layout",
            "Split ratio",
            "Default share of the first tile.",
            Kind::Float {
                min: Some(0.2),
                max: Some(0.8),
            },
            json!(0.5),
            Immediate,
            prefs,
            &["split", "tile"],
        ),
        setting(
            SETTINGS_BOUNDS_KEY,
            Desktop,
            General,
            "Window",
            "Settings window bounds",
            "Position and size of the Settings window, saved when it closes.",
            Kind::Json { max_bytes: 128 },
            Value::Null,
            Immediate,
            prefs,
            &["window"],
        ),
        setting(
            "lotus.desktop.discord_presence",
            Desktop,
            Discord,
            "Rich Presence",
            "Discord Rich Presence",
            "Show what is running in Discord.",
            Kind::Boolean,
            json!(false),
            Immediate,
            prefs,
            &["discord", "presence"],
        ),
        setting(
            "lotus.desktop.discord_hide_titles",
            Desktop,
            Discord,
            "Rich Presence",
            "Hide game titles",
            "Report \"Playing a game\" instead of file names.",
            Kind::Boolean,
            json!(false),
            Immediate,
            prefs,
            &["discord", "privacy"],
        ),
        setting(
            "lotus.desktop.discord_application_id",
            Desktop,
            Discord,
            "Rich Presence",
            "Discord application ID",
            "Overrides the built-in application ID.",
            Kind::String {
                max_len: 32,
                choices: None,
                nullable: true,
                exact_len: None,
                hex: false,
            },
            Value::Null,
            Immediate,
            prefs,
            &["discord"],
        ),
    ]
    .into_iter()
    .chain(input_descriptors())
    .chain(debug_descriptors())
    .collect()
}
