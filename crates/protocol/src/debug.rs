//! Validated operations shared by every debugger surface.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SettingValue {
    Boolean(bool),
    Integer(i64),
    Number(f64),
    Text(String),
    Array(Vec<SettingValue>),
    Object(std::collections::BTreeMap<String, SettingValue>),
}

/// Unknown operations and fields fail before reaching a worker.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum DebugCommand {
    Status,
    Load {
        asset: String,
    },
    Reset,
    Stop,
    SettingsList {
        #[serde(default)]
        offset: u32,
        #[serde(default = "settings_page_size")]
        limit: u32,
    },
    SettingsGet {
        name: String,
    },
    SettingsSet {
        name: String,
        value: SettingValue,
    },
    SettingsDiff,
    Registers {
        core: u32,
    },
    WriteRegister {
        core: u32,
        register: u32,
        value: u32,
    },
    ReadMemory {
        address: u32,
        length: u32,
    },
    WriteMemory {
        address: u32,
        bytes: Vec<u8>,
    },
    Step {
        core: u32,
        count: u32,
    },
    RunTo {
        core: u32,
        address: u32,
    },
    StepOver {
        core: u32,
    },
    StepOut {
        core: u32,
    },
    Handles,
    Disassemble {
        address: u32,
        length: u32,
        thumb: bool,
    },
    GpuConfigure {
        enabled: bool,
        stop_events: Vec<u32>,
        /// Emulation-thread cost of capture (worker default `full`). `counters` keeps the
        /// batched PICA command path and accepts no stop events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capture: Option<GpuCapture>,
        /// Inclusive `[first, last]` draw ids snapshotted in `draws` mode.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        draw_range: Option<[u64; 2]>,
        /// Snapshot budget in `draws` mode (1..=4096, worker default 8); later draws are
        /// counted as `snapshots_skipped_budget`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_draws: Option<u32>,
        /// Copy texture/index/vertex bytes into `draws` snapshots (always on in `full`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resources: Option<bool>,
    },
    GpuDraws,
    CitraceStart,
    CitraceStop,
    CitraceStatus,
    CitraceReplay,
    GpuDraw {
        draw: u64,
    },
    ShaderTrace {
        draw: u64,
        vertex: u32,
    },
    Surface {
        address: u32,
        width: u32,
        height: u32,
        format: u32,
    },
    LoadSymbols {
        asset: String,
    },
    Symbolize {
        addresses: Vec<u32>,
    },
    ResolveSymbol {
        query: String,
    },
    /// Execute breakpoints at every statement start of `file:line` in the bound ELF.
    /// The host keeps the source location and re-resolves it for each new binding.
    SourceBreakpointSet {
        file: String,
        line: u32,
    },
    SourceBreakpointRemove {
        id: u64,
    },
    SourceBreakpoints,
    /// Step to the next statement start of a different line, within an instruction budget.
    SourceStep {
        core: u32,
        mode: SourceStepMode,
        max_instructions: u32,
    },
    /// Lines of a bound ELF's source file, read only below a configured source root.
    SourceExcerpt {
        file: String,
        line: u32,
        context: u32,
    },
    Deadlocks,
    CrashReport {
        core: u32,
    },
    Scan {
        address: u32,
        length: u32,
        width: u32,
        value: u64,
    },
    ScanNarrow {
        mode: ScanMode,
        value: Option<u64>,
        upper: Option<u64>,
    },
    ScanResults {
        offset: u32,
        limit: u32,
    },
    PointerChain {
        base: u32,
        offsets: Vec<i32>,
    },
    BreakpointConfigure {
        id: u64,
        process: Option<u32>,
        hit_after: u64,
        condition: Option<Condition>,
        log: Option<String>,
    },
    Threads,
    ThreadRegisters {
        thread: u32,
    },
    WriteThreadRegister {
        thread: u32,
        register: u32,
        value: u32,
    },
    HioForward {
        enabled: bool,
    },
    HioReply {
        value: i64,
        errno: u32,
        interrupted: bool,
    },
    Processes,
    MemoryMap,
    Suspend {
        thread: Option<u32>,
        suspended: bool,
    },
    Backtrace {
        core: u32,
        limit: u32,
    },
    BreakpointSet {
        address: u32,
        kind: BreakpointKind,
        length: u32,
    },
    BreakpointRemove {
        id: u64,
    },
    Breakpoints,
    SaveState {
        name: String,
    },
    LoadState {
        name: String,
    },
    States,
    Logs {
        after: u64,
        limit: u32,
    },
    Events {
        after: u64,
        limit: u32,
    },
    Perf,
    AudioFocus {
        enabled: bool,
    },
    AudioCapture {
        enabled: bool,
    },
    InputState {
        owner: String,
        state: PeripheralInput,
    },
    AssetMount {
        asset: String,
        kind: AssetKind,
    },
    NfcRemove,
    DspInspect,
    DspLoadFirmware,
    Fork {
        count: u8,
        #[serde(default)]
        overrides: std::collections::BTreeMap<String, SettingValue>,
    },
    RewindConfigure {
        enabled: bool,
        capacity: u32,
        budget_mib: u32,
        /// Frames between continuous-run checkpoints (1..=60, default 1). Larger values cut
        /// savestate cost; reverse replay then re-executes longer windows. Input changes
        /// always force a checkpoint.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interval_frames: Option<u32>,
        /// zstd level of stored checkpoints (-7..=19, worker default 3). Every level restores
        /// the same state; lower levels trade memory for checkpoint time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compression_level: Option<i8>,
        /// Checkpoints waiting for or undergoing background compression (0..=4, worker default
        /// 1). Each holds its serialized state until compressed. A full queue makes the next
        /// checkpoint wait. 0 compresses on the emulation thread.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(range(max = 4))]
        compression_queue: Option<u8>,
    },
    RewindStatus,
    ReverseStep {
        core: u32,
    },
    ReverseContinue,
    RunUntil {
        predicate: Predicate,
        frames: u16,
        timeout_ms: u32,
    },
    CaptureScaled {
        scale: u32,
    },
    MovieRecord {
        name: String,
    },
    MoviePlay {
        name: String,
        read_only: bool,
    },
    MovieStop,
    MovieStatus,
    Matrix {
        profiles: Vec<MatrixProfile>,
        frames: u16,
        checkpoints: Vec<u16>,
        repeats: u8,
    },
    StorageBrowse {
        area: StorageArea,
        path: String,
        offset: u32,
        limit: u32,
    },
    InstallCia {
        asset: String,
    },
    ContentTransform {
        asset: String,
        compress: bool,
    },
    BootHomeMenu {
        region: u32,
    },
    Titles {
        media: u32,
        offset: u32,
        limit: u32,
    },
    SystemRegenerate {
        console_id: bool,
        mac: bool,
    },
    Cheats,
    CheatAdd {
        name: String,
        code: String,
        comments: String,
        enabled: bool,
    },
    CheatEnable {
        index: u32,
        enabled: bool,
    },
    CheatRemove {
        index: u32,
    },
    Continue,
    Pause,
    Advance {
        frames: u16,
    },
    LeaseAcquire {
        owner: String,
    },
    LeaseRelease {
        owner: String,
    },
    Trace {
        enabled: bool,
    },
    /// Which event categories the worker pushes as notifications and keeps for `events`/`logs`
    /// (CITnLO, lotus#26). An omitted field keeps the worker's current set; stops are always
    /// kept. Unsubscribed categories are not built at all.
    EventsConfigure {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notify: Option<Vec<EventCategory>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        history: Option<Vec<EventCategory>>,
    },
    /// Hashes of physical memory, per-core context and ticks, and (when no async HLE work is
    /// pending) each `System::serialize` section, for finding where two sessions diverge
    /// (CITnLO, lotus#29). Paused only. `dump` also writes the serialized bytes and section
    /// offsets to `<root>/state-dumps/<dump>.bin` and `.json`.
    StateHash {
        #[serde(default = "state_hash_chunk_kib")]
        chunk_kib: u32,
        #[serde(default = "default_true")]
        sections: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dump: Option<String>,
    },
}

/// Worker event categories (CITnLO `debug.h` `DebugTools::CATEGORIES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    /// `stopped`: debug stops, including the per-run `frame` stop.
    Stopped,
    /// `guest_log` (svcOutputDebugString), `hio_stdout`, `logpoint`.
    Log,
    /// Emulator log entries that pass the log filter.
    NativeLog,
    /// `svc_enter`/`svc_exit` while `trace` or an SVC predicate is active.
    Svc,
    /// `ipc` while `trace` is enabled.
    Ipc,
    /// `pica` while `gpu_configure` captures `events` or more.
    Pica,
}

/// How much a GPU capture records per draw (CITnLO `debug_gpu.h`, lotus#24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GpuCapture {
    /// PicaCore command-list, draw and fast-path counters; no debug context.
    Counters,
    /// Per-command PICA events and draw metadata.
    Events,
    /// Events plus register/shader snapshots of selected draws.
    Draws,
    /// Every draw snapshotted with its resources.
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceStepMode {
    /// Stop at the next statement, entering calls that have line information.
    Into,
    /// Stop at the next statement in this frame or a caller; calls run to their return.
    Over,
    /// Run to the caller, then report its location.
    Out,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BreakpointKind {
    Execute,
    Read,
    Write,
    Access,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Condition {
    RegisterEquals {
        register: u32,
        value: u32,
        mask: u32,
    },
    MemoryEquals {
        address: u32,
        width: u32,
        value: u64,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScanMode {
    Exact,
    Changed,
    Unchanged,
    Range,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    DspFirmware,
    Otp,
    SecureInfo,
    LocalFriendCodeSeed,
    Movable,
    MicWav,
    CameraBmp,
    Nfc,
    Cia,
    Rom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageArea {
    Nand,
    Sdmc,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Predicate {
    Pc {
        core: u32,
        address: u32,
    },
    Memory {
        core: u32,
        address: u32,
        width: u32,
        value: u64,
    },
    Svc {
        id: u32,
        enter: bool,
    },
    Log {
        regex: String,
    },
    Frame {
        frame: u64,
    },
    Draw {
        draw: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MatrixProfile {
    pub name: String,
    /// A configured worker handle; never an executable path.
    pub backend: Option<String>,
    pub settings: std::collections::BTreeMap<String, SettingValue>,
}

/// Native input units: analog [-1,1], acceleration g, gyro degrees/second.
/// Motion axes are +left, +out of touchscreen, +up (core/frontend/input.h).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PeripheralInput {
    pub buttons: u32,
    pub x: u16,
    pub y: u16,
    pub touch: bool,
    pub circle_pad: [f32; 2],
    pub c_stick: [f32; 2],
    pub acceleration: [f32; 3],
    pub gyroscope: [f32; 3],
    pub slider_3d: f32,
    pub volume: f32,
}

fn settings_page_size() -> u32 {
    32
}

fn state_hash_chunk_kib() -> u32 {
    1024
}

fn default_true() -> bool {
    true
}

impl DebugCommand {
    /// The worker feature ([`crate::capabilities::feature`]) this command needs beyond the base
    /// debug API, if any. `gpu_configure` without `capture` is the base full capture.
    #[must_use]
    pub fn required_feature(&self) -> Option<&'static str> {
        use crate::capabilities::feature;
        match self {
            Self::GpuConfigure {
                capture: Some(_), ..
            } => Some(feature::GPU_CAPTURE_LEVELS),
            Self::RewindConfigure {
                compression_queue: Some(_),
                ..
            } => Some(feature::REWIND_COMPRESSION_QUEUE),
            Self::EventsConfigure { .. } => Some(feature::EVENT_SUBSCRIPTIONS),
            Self::StateHash { .. } => Some(feature::STATE_HASH),
            Self::SettingsSet { name, .. } if name == "deterministic_async_operations" => {
                Some(feature::DETERMINISTIC_ASYNC)
            }
            _ => None,
        }
    }
}

impl DebugCommand {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::BootHomeMenu { region } if *region > 6 => Err("HOME region 0..6"),
            Self::Titles { media, limit, .. } if *media > 2 || !(1..=128).contains(limit) => {
                Err("titles media 0..2, limit 1..128")
            }
            Self::StorageBrowse { path, limit, .. }
                if path.len() > 1024 || !(1..=128).contains(limit) =>
            {
                Err("storage path/page budget")
            }
            Self::CheatAdd {
                name,
                code,
                comments,
                ..
            } if name.is_empty()
                || name.len() > 128
                || code.is_empty()
                || code.len() > 8192
                || comments.len() > 1024 =>
            {
                Err("cheat name/code/comment budget")
            }
            Self::Matrix {
                profiles,
                frames,
                checkpoints,
                repeats,
            } => {
                if !(2..=8).contains(&profiles.len())
                    || !(1..=3).contains(repeats)
                    || !(1..=600).contains(frames)
                    || checkpoints.is_empty()
                    || checkpoints.len() > 32
                    || checkpoints.iter().any(|f| *f == 0 || f > frames)
                    || checkpoints.windows(2).any(|p| p[0] >= p[1])
                {
                    return Err("matrix requires 2..8 profiles, 1..3 repeats, ordered checkpoints within 1..600 frames");
                }
                for profile in profiles {
                    if profile.name.is_empty()
                        || profile.name.len() > 64
                        || profile.settings.len() > 64
                    {
                        return Err("matrix profile exceeds limits");
                    }
                    for (name, value) in &profile.settings {
                        Self::SettingsSet {
                            name: name.clone(),
                            value: value.clone(),
                        }
                        .validate()?;
                    }
                }
                Ok(())
            }
            Self::CaptureScaled { scale } if !(1..=4).contains(scale) => Err("capture scale 1..4"),
            Self::RunUntil {
                predicate,
                frames,
                timeout_ms,
            } => {
                if !(1..=600).contains(frames) || !(1..=20_000).contains(timeout_ms) {
                    return Err("predicate run budget: 1..600 frames, 1..20000 host milliseconds");
                }
                match predicate {
                    Predicate::Pc { core, .. } | Predicate::Memory { core, .. } if *core > 3 => {
                        Err("invalid predicate core")
                    }
                    Predicate::Memory { address, width, .. } if matches!(width, 1 | 2 | 4 | 8) => {
                        range(*address, *width, 8)
                    }
                    Predicate::Memory { .. } => Err("predicate memory width 1/2/4/8"),
                    Predicate::Svc { id, .. } if *id > 255 => Err("invalid SVC predicate"),
                    Predicate::Log { regex } if regex.is_empty() || regex.len() > 256 => {
                        Err("log regex length 1..256")
                    }
                    _ => Ok(()),
                }
            }
            Self::RewindConfigure {
                capacity,
                budget_mib,
                ..
            } if !(1..=128).contains(capacity) || !(16..=256).contains(budget_mib) => {
                Err("rewind capacity 1..128, memory budget 16..256 MiB")
            }
            Self::RewindConfigure {
                interval_frames: Some(interval),
                ..
            } if !(1..=60).contains(interval) => Err("rewind interval 1..60 frames"),
            Self::RewindConfigure {
                compression_level: Some(level),
                ..
            } if !(-7..=19).contains(level) => Err("rewind compression_level -7..19"),
            Self::RewindConfigure {
                compression_queue: Some(queue),
                ..
            } if *queue > 4 => Err("rewind compression_queue 0..4"),
            Self::ReverseStep { core } if *core > 3 => Err("invalid core"),
            Self::Fork { count, overrides } if !(1..=3).contains(count) || overrides.len() > 32 => {
                Err("fork requires 1..3 siblings and at most 32 overrides")
            }
            Self::InputState { owner, state } => {
                if owner != "live" && owner != "scenario" {
                    return Err("input owner live or scenario");
                }
                if state.buttons >= (1 << 18)
                    || state.x >= 320
                    || state.y >= 240
                    || state
                        .circle_pad
                        .iter()
                        .chain(state.c_stick.iter())
                        .any(|v| !v.is_finite() || v.abs() > 1.)
                    || state
                        .acceleration
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > 16.)
                    || state
                        .gyroscope
                        .iter()
                        .any(|v| !v.is_finite() || v.abs() > 4000.)
                    || !state.slider_3d.is_finite()
                    || !(0. ..=1.).contains(&state.slider_3d)
                    || !state.volume.is_finite()
                    || !(0. ..=1.).contains(&state.volume)
                {
                    return Err("invalid peripheral input range");
                }
                Ok(())
            }
            Self::ScanNarrow { mode, value, upper } => match mode {
                ScanMode::Exact if value.is_none() => Err("exact scan requires value"),
                ScanMode::Range if value.is_none() || upper.is_none() || value > upper => {
                    Err("scan requires ordered range")
                }
                _ => Ok(()),
            },
            Self::SettingsList { offset, limit } if *offset > 512 || !(1..=32).contains(limit) => {
                Err("invalid settings page")
            }
            Self::SettingsSet { name, value }
                if name.len() > 128
                    || serde_json::to_vec(value)
                        .map_err(|_| "invalid setting value")?
                        .len()
                        > 8192 =>
            {
                Err("setting name or value exceeds budget")
            }
            Self::GpuConfigure { stop_events, .. }
                if stop_events.len() > 8 || stop_events.iter().any(|e| *e > 7) =>
            {
                Err("PICA events 0..7")
            }
            Self::GpuConfigure {
                capture: Some(GpuCapture::Counters),
                stop_events,
                ..
            } if !stop_events.is_empty() => Err("stop_events need capture events or more"),
            Self::GpuConfigure {
                draw_range: Some([first, last]),
                ..
            } if first > last => Err("draw_range: [first, last]"),
            Self::GpuConfigure {
                max_draws: Some(budget),
                ..
            } if !(1..=4096).contains(budget) => Err("max_draws 1..4096"),
            Self::Surface {
                width,
                height,
                format,
                ..
            } if *width == 0
                || *height == 0
                || *width > 1024
                || *height > 1024
                || width % 8 != 0
                || height % 8 != 0
                || *format > 13 =>
            {
                Err("invalid texture surface")
            }
            Self::Symbolize { addresses } if addresses.len() > 128 => Err("symbolize cap 128"),
            Self::SourceBreakpointSet { file, line } | Self::SourceExcerpt { file, line, .. }
                if file.is_empty() || file.len() > 1024 || *line == 0 =>
            {
                Err("source file 1..1024 bytes and line from 1")
            }
            Self::SourceExcerpt { context, .. } if *context > 32 => Err("excerpt context 0..32"),
            Self::SourceStep {
                core,
                max_instructions,
                ..
            } if *core > 3 || !(1..=100_000).contains(max_instructions) => {
                Err("source step core 0..3, max_instructions 1..100000")
            }
            Self::ResolveSymbol { query } if query.len() > 1024 => {
                Err("symbol query exceeds budget")
            }
            Self::RunTo { core, .. }
            | Self::StepOver { core }
            | Self::StepOut { core }
            | Self::CrashReport { core }
                if *core > 3 =>
            {
                Err("invalid core")
            }
            Self::Scan {
                address,
                length,
                width,
                ..
            } => {
                range(*address, *length, 1 << 20)?;
                if !matches!(width, 1 | 2 | 4 | 8) || length % width != 0 {
                    return Err("invalid scan width");
                }
                Ok(())
            }
            Self::ScanResults { limit, .. } if !(1..=256).contains(limit) => {
                Err("invalid scan page")
            }
            Self::PointerChain { offsets, .. } if offsets.len() > 32 => {
                Err("pointer chain exceeds 32 hops")
            }
            Self::BreakpointConfigure { condition, log, .. } => {
                if log.as_ref().is_some_and(|s| s.len() > 512) {
                    return Err("logpoint text exceeds budget");
                }
                match condition {
                    Some(Condition::RegisterEquals { register, .. }) if *register > 16 => {
                        Err("condition register 0..16")
                    }
                    Some(Condition::MemoryEquals { address, width, .. })
                        if matches!(width, 1 | 2 | 4 | 8) =>
                    {
                        range(*address, *width, 8)
                    }
                    Some(Condition::MemoryEquals { .. }) => Err("invalid condition width"),
                    _ => Ok(()),
                }
            }
            Self::Advance { frames } if !(1..=600).contains(frames) => Err("advance 1..600 frames"),
            Self::Disassemble {
                address, length, ..
            }
            | Self::ReadMemory { address, length } => range(*address, *length, 4096),
            Self::WriteMemory { address, bytes } => range(
                *address,
                bytes.len().try_into().map_err(|_| "write too large")?,
                4096,
            ),
            Self::Step { core, count } if *core > 3 || !(1..=100_000).contains(count) => {
                Err("step requires core 0..3 and count 1..100000")
            }
            Self::Registers { core }
            | Self::WriteRegister { core, .. }
            | Self::Backtrace { core, .. }
                if *core > 3 =>
            {
                Err("invalid core")
            }
            Self::WriteRegister { register, .. } | Self::WriteThreadRegister { register, .. }
                if *register > 82 =>
            {
                Err("invalid register")
            }
            Self::Backtrace { limit, .. } if !(1..=128).contains(limit) => {
                Err("backtrace limit 1..128")
            }
            Self::BreakpointSet {
                address,
                length,
                kind,
            } => {
                range(*address, *length, 65536)?;
                if *kind == BreakpointKind::Execute && !matches!(length, 2 | 4) {
                    return Err("execution breakpoint length must be 2 or 4");
                }
                Ok(())
            }
            Self::SaveState { name }
            | Self::LoadState { name }
            | Self::MovieRecord { name }
            | Self::MoviePlay { name, .. }
                if name.is_empty()
                    || name.len() > 32
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') =>
            {
                Err("state name must be 1..32 letters, digits, '_' or '-'")
            }
            Self::Events { limit, .. } | Self::Logs { limit, .. } if !(1..=128).contains(limit) => {
                Err("stream limit 1..128")
            }
            // Lotus's GDB server waits for pushed `stopped` notifications (crates/gdb wait_for_stop).
            Self::EventsConfigure {
                notify: Some(notify),
                ..
            } if !notify.contains(&EventCategory::Stopped) => {
                Err("notify must include stopped; Lotus's debugger waits for it")
            }
            Self::StateHash { chunk_kib, .. }
                if !(4..=65536).contains(chunk_kib) || !chunk_kib.is_power_of_two() =>
            {
                Err("chunk_kib must be a power of two, 4..65536")
            }
            Self::StateHash {
                dump: Some(name), ..
            } if name.is_empty()
                || name.len() > 32
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') =>
            {
                Err("dump name must be 1..32 letters, digits, '_' or '-'")
            }
            _ => Ok(()),
        }
    }

    pub fn mutates(&self) -> bool {
        matches!(
            self,
            Self::CitraceStart
                | Self::CitraceStop
                | Self::CitraceReplay
                | Self::GpuConfigure { .. }
                | Self::ShaderTrace { .. }
                | Self::Surface { .. }
                | Self::LoadSymbols { .. }
                | Self::SourceBreakpointSet { .. }
                | Self::SourceBreakpointRemove { .. }
                | Self::SourceStep { .. }
                | Self::RunTo { .. }
                | Self::StepOver { .. }
                | Self::StepOut { .. }
                | Self::BreakpointConfigure { .. }
                | Self::Scan { .. }
                | Self::ScanNarrow { .. }
                | Self::WriteThreadRegister { .. }
                | Self::HioForward { .. }
                | Self::HioReply { .. }
                | Self::SettingsSet { .. }
                | Self::WriteRegister { .. }
                | Self::DspLoadFirmware
                | Self::InstallCia { .. }
                | Self::ContentTransform { .. }
                | Self::BootHomeMenu { .. }
                | Self::SystemRegenerate { .. }
                | Self::CheatAdd { .. }
                | Self::CheatEnable { .. }
                | Self::CheatRemove { .. }
                | Self::Load { .. }
                | Self::Reset
                | Self::Stop
                | Self::Matrix { .. }
                | Self::MovieRecord { .. }
                | Self::MoviePlay { .. }
                | Self::MovieStop
                | Self::RunUntil { .. }
                | Self::RewindConfigure { .. }
                | Self::ReverseStep { .. }
                | Self::ReverseContinue
                | Self::Fork { .. }
                | Self::AssetMount { .. }
                | Self::NfcRemove
                | Self::InputState { .. }
                | Self::WriteMemory { .. }
                | Self::Step { .. }
                | Self::Suspend { .. }
                | Self::BreakpointSet { .. }
                | Self::BreakpointRemove { .. }
                | Self::LoadState { .. }
                | Self::SaveState { .. }
                | Self::Trace { .. }
                | Self::EventsConfigure { .. }
                | Self::StateHash { dump: Some(_), .. }
                | Self::Continue
                | Self::Pause
                | Self::Advance { .. }
                | Self::LeaseAcquire { .. }
                | Self::LeaseRelease { .. }
        )
    }
}

fn range(address: u32, length: u32, maximum: u32) -> Result<(), &'static str> {
    if length == 0 || length > maximum || u64::from(address) + u64::from(length) > 1u64 << 32 {
        Err("invalid or oversized guest range")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_commands_are_rejected_before_transport() {
        assert!(DebugCommand::ReadMemory {
            address: u32::MAX,
            length: 2
        }
        .validate()
        .is_err());
        assert!(DebugCommand::LoadState {
            name: "../escape".into()
        }
        .validate()
        .is_err());
        assert!(serde_json::from_str::<DebugCommand>(
            r#"{"operation":"step","core":0,"count":1,"ignored":true}"#
        )
        .is_err());
        assert!(DebugCommand::ReadMemory {
            address: u32::MAX,
            length: 1
        }
        .validate()
        .is_ok());
        let rewind = |level| DebugCommand::RewindConfigure {
            enabled: true,
            capacity: 32,
            budget_mib: 128,
            interval_frames: None,
            compression_level: level,
            compression_queue: None,
        };
        assert!(rewind(Some(20)).validate().is_err());
        assert!(rewind(Some(-8)).validate().is_err());
        assert!(rewind(Some(-7)).validate().is_ok());
        assert!(rewind(None).validate().is_ok());
        let queue = |queue| DebugCommand::RewindConfigure {
            enabled: true,
            capacity: 32,
            budget_mib: 128,
            interval_frames: None,
            compression_level: None,
            compression_queue: queue,
        };
        assert!(queue(Some(0)).validate().is_ok());
        assert!(queue(Some(4)).validate().is_ok());
        assert!(queue(Some(5)).validate().is_err());
        let gpu = |capture, stop_events, draw_range, max_draws| DebugCommand::GpuConfigure {
            enabled: true,
            stop_events,
            capture,
            draw_range,
            max_draws,
            resources: None,
        };
        assert!(gpu(Some(GpuCapture::Counters), vec![2], None, None)
            .validate()
            .is_err());
        assert!(gpu(Some(GpuCapture::Events), vec![2], None, None)
            .validate()
            .is_ok());
        assert!(gpu(Some(GpuCapture::Draws), vec![], Some([5, 4]), None)
            .validate()
            .is_err());
        assert!(gpu(Some(GpuCapture::Draws), vec![], Some([4, 4]), Some(0))
            .validate()
            .is_err());
        assert!(gpu(None, vec![], None, Some(4096)).validate().is_ok());
        // Clients that omit the new fields keep sending the v0.0.3 request.
        assert_eq!(
            serde_json::to_value(gpu(None, vec![], None, None)).expect("json"),
            serde_json::json!({"operation": "gpu_configure", "enabled": true, "stop_events": []})
        );
    }

    #[test]
    fn events_configure_keeps_stops_and_omits_unset_fields() {
        let configure = |notify, history| DebugCommand::EventsConfigure { notify, history };
        assert!(configure(Some(vec![EventCategory::Log]), None)
            .validate()
            .is_err());
        assert!(configure(
            Some(vec![EventCategory::Stopped, EventCategory::Log]),
            Some(vec![])
        )
        .validate()
        .is_ok());
        assert_eq!(
            serde_json::to_value(configure(None, Some(vec![EventCategory::NativeLog])))
                .expect("json"),
            serde_json::json!({"operation": "events_configure", "history": ["native_log"]})
        );
        assert!(serde_json::from_value::<DebugCommand>(
            serde_json::json!({"operation": "events_configure", "notify": ["gpu"]})
        )
        .is_err());
    }

    #[test]
    fn state_hash_defaults_bounds_and_dump_mutation() {
        let parse = |value| serde_json::from_value::<DebugCommand>(value).expect("parse");
        let plain = parse(serde_json::json!({"operation": "state_hash"}));
        assert!(matches!(
            plain,
            DebugCommand::StateHash {
                chunk_kib: 1024,
                sections: true,
                dump: None
            }
        ));
        assert!(plain.validate().is_ok() && !plain.mutates());
        for chunk_kib in [0, 2, 1000, 131072] {
            let command =
                parse(serde_json::json!({"operation": "state_hash", "chunk_kib": chunk_kib}));
            assert!(command.validate().is_err(), "{chunk_kib}");
        }
        let dump = parse(serde_json::json!({"operation": "state_hash", "dump": "run-0"}));
        assert!(dump.validate().is_ok() && dump.mutates());
        for name in ["", "../x", "a b"] {
            let command = parse(serde_json::json!({"operation": "state_hash", "dump": name}));
            assert!(command.validate().is_err(), "{name:?}");
        }
    }
}
