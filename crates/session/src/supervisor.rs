//! Local host policy shared by the headless MCP server and the desktop.
//!
//! One `Supervisor` owns every session of a LOTUS process. Each session has its
//! own async lock and its own supervision and input tasks ([`supervision`]), so a
//! slow or stalled worker never delays commands, status, input or continuation
//! of another. The registry lock is never held across an await. Desktop and MCP
//! call the same methods; there is one control model.
//!
//! Sessions are *primary* (mount a persistent card) or *secondary* (private
//! scratch copy, discarded on end); see [`crate::card_store`].
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub use crate::artifacts::{Artifact, ArtifactQuota};
use crate::artifacts::{ArtifactStore, NewArtifact};
use crate::card_store::{CardStore, ScratchEnd};
use crate::native_session::{Frame, InputState, NativeSession};
use crate::telemetry::{Actor, Telemetry};
use crate::SessionState;
use lotus_protocol::envelope::SessionUuid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
mod experiments;
mod supervision;

/// Cancellation is serialized with the transition from waiting for admission to
/// dispatch. Once dispatch starts, the worker exchange must drain to its reply.
#[derive(Clone, Debug)]
pub struct DebugCancellation(tokio::sync::watch::Sender<DebugPhase>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DebugPhase {
    Waiting,
    Dispatched,
    Cancelled,
    Discarded,
}

impl Default for DebugCancellation {
    fn default() -> Self {
        Self(tokio::sync::watch::Sender::new(DebugPhase::Waiting))
    }
}

impl DebugCancellation {
    /// Returns whether the operation crossed its dispatch boundary.
    pub fn cancel(&self) -> bool {
        let mut dispatched = false;
        self.0.send_if_modified(|phase| match *phase {
            DebugPhase::Waiting => {
                *phase = DebugPhase::Cancelled;
                true
            }
            DebugPhase::Dispatched => {
                dispatched = true;
                *phase = DebugPhase::Discarded;
                true
            }
            DebugPhase::Cancelled => false,
            DebugPhase::Discarded => {
                dispatched = true;
                false
            }
        });
        dispatched
    }

    pub fn check(&self) -> io::Result<()> {
        match *self.0.borrow() {
            DebugPhase::Cancelled | DebugPhase::Discarded => Err(debug_cancelled()),
            DebugPhase::Waiting | DebugPhase::Dispatched => Ok(()),
        }
    }

    fn dispatch(&self) -> io::Result<()> {
        let changed = self.0.send_if_modified(|phase| {
            if *phase != DebugPhase::Waiting {
                return false;
            }
            *phase = DebugPhase::Dispatched;
            true
        });
        if changed {
            Ok(())
        } else {
            Err(debug_cancelled())
        }
    }
}

fn debug_cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "debugger request cancelled")
}

#[derive(Clone, Copy, Default)]
pub enum DebugPrecondition<'a> {
    #[default]
    None,
    Paused,
    MemoryBytes(&'a [u8]),
    ScanGeneration(u64),
    Thread {
        process: u32,
        thread: u32,
    },
}

#[derive(Default)]
struct DebugScope<'a> {
    target: Option<(u64, u64)>,
    cancellation: Option<&'a DebugCancellation>,
    precondition: DebugPrecondition<'a>,
}

impl DebugScope<'_> {
    fn dispatch(&self) -> io::Result<()> {
        self.cancellation
            .map_or(Ok(()), DebugCancellation::dispatch)
    }
}

/// Host policy covers audio controls as well as guest writes.
pub fn debug_requires_control(command: &lotus_protocol::debug::DebugCommand) -> bool {
    use lotus_protocol::debug::DebugCommand;
    command.mutates()
        || matches!(
            command,
            DebugCommand::AudioFocus { .. } | DebugCommand::AudioCapture { .. }
        )
}

/// Sessions per host unless the configuration raises it; four is the tested load.
pub const DEFAULT_MAX_SESSIONS: usize = 4;
/// Upper bound accepted for `max_sessions`.
pub const MAX_SESSIONS_LIMIT: usize = 16;
/// Per-worker stop deadline, including the wait for a session held by a stalled exchange.
const CLOSE_DEADLINE: Duration = Duration::from_secs(3);
/// Longest wait of a card copy for its primary's creation to commit or fail.
const MOUNT_WAIT: Duration = Duration::from_secs(30);
/// Frames a running primary may advance while waiting for its guest FS work to settle.
const QUIESCE_FRAMES: u32 = 30;
/// After terminating a worker's job, how long its in-flight exchange may take to fail.
const RELEASE_DEADLINE: Duration = Duration::from_secs(2);

/// Host configuration, never accepted as a tool-call argument.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub worker: PathBuf,
    pub storage_root: PathBuf,
    /// Approved content, by handle.
    #[serde(default)]
    pub assets: BTreeMap<String, PathBuf>,
    /// Approved directories: every `.3dsx` directly inside is `<handle>/<file>`.
    #[serde(default)]
    pub asset_directories: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub allow_control: bool,
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    /// Validated worker settings applied before loading content.
    #[serde(default)]
    pub profile: BTreeMap<String, lotus_protocol::debug::SettingValue>,
    #[serde(default)]
    pub workers: BTreeMap<String, PathBuf>,
    /// New sessions whose `profile` names no `graphics_api` use Vulkan, falling back to
    /// software (reason in telemetry) when the worker refuses Vulkan setup. Default true for
    /// every frontend; `false` keeps the worker default (software). Pin a renderer with
    /// `profile.graphics_api` (0 software, 2 Vulkan) for reference or determinism runs.
    #[serde(default = "default_prefer_vulkan")]
    pub prefer_vulkan: bool,
    /// Artifact count/byte quota, per-session floor and lease ([`crate::artifacts`]).
    #[serde(default)]
    pub artifacts: ArtifactQuota,
    /// Seed each new worker root's `shaders/` from the last clean stop of the same worker
    /// binary ([`crate::shader_seed`]). Off by default until measured; off for cold-start or
    /// cache-sensitive benchmarks.
    #[serde(default)]
    pub shader_seed: bool,
    /// Interactive or batch workload, expensive-tool budgets and memory admission
    /// ([`crate::policy`]).
    #[serde(default)]
    pub policy: crate::policy::Policy,
}

fn default_prefer_vulkan() -> bool {
    true
}

fn default_max_sessions() -> usize {
    DEFAULT_MAX_SESSIONS
}

impl HostConfig {
    /// Configuration with no assets, control denied and the default session limit.
    pub fn new(worker: impl Into<PathBuf>, storage_root: impl Into<PathBuf>) -> Self {
        Self {
            worker: worker.into(),
            storage_root: storage_root.into(),
            assets: BTreeMap::new(),
            asset_directories: BTreeMap::new(),
            allow_control: false,
            max_sessions: DEFAULT_MAX_SESSIONS,
            profile: BTreeMap::new(),
            workers: BTreeMap::new(),
            prefer_vulkan: true,
            artifacts: ArtifactQuota::default(),
            shader_seed: false,
            policy: crate::policy::Policy::default(),
        }
    }
}

/// Check `HostConfig.profile` against the settings registry (lotus#50): every key must be an
/// Azahar worker setting that is not excluded, settable globally, with a valid value. The
/// configured workers' branch is not known before they start, so a CITnLO-only key passes
/// here and the AZAHAR worker refuses it at `settings_set`.
pub fn validate_profile(
    profile: &BTreeMap<String, lotus_protocol::debug::SettingValue>,
) -> io::Result<()> {
    use lotus_settings::{Branch, Owner, Registry, Scope};
    let invalid = |message: String| {
        io::Error::new(io::ErrorKind::InvalidInput, format!("profile: {message}"))
    };
    for (key, value) in profile {
        let value = serde_json::to_value(value).map_err(io::Error::other)?;
        let registry = Registry::builtin();
        let checked = registry
            .validate(key, &value, Scope::Global, Some(Branch::Integration))
            .or_else(|_| registry.validate(key, &value, Scope::Global, Some(Branch::Experimental)))
            .map_err(|error| invalid(error.to_string()))?;
        if checked.owner != Owner::Azahar {
            return Err(invalid(format!(
                "{key} is a Lotus setting, not a worker setting"
            )));
        }
    }
    Ok(())
}

/// An input-layout key (lotus#63): layered like worker settings, never sent to a worker.
fn is_input(key: &str) -> bool {
    lotus_settings::Registry::builtin()
        .get(key)
        .is_some_and(|d| d.owner == lotus_settings::Owner::Input)
}

fn same_setting(key: &str, left: &serde_json::Value, right: &serde_json::Value) -> bool {
    if left == right {
        return true;
    }
    // Native float settings are backed by single-precision settings bindings.
    if lotus_settings::Registry::builtin()
        .get(key)
        .is_some_and(|descriptor| matches!(descriptor.kind, lotus_settings::Kind::Float { .. }))
    {
        return matches!((left.as_f64(), right.as_f64()),
            (Some(left), Some(right)) if left as f32 == right as f32);
    }
    false
}

/// Parse a `--asset-dir handle=path` command-line override.
pub fn parse_asset_dir(argument: &str) -> Result<(String, PathBuf), String> {
    match argument.split_once('=') {
        Some((handle, path)) if !handle.is_empty() && !path.is_empty() => {
            Ok((handle.to_string(), PathBuf::from(path)))
        }
        _ => Err(format!("--asset-dir expects handle=path, got {argument:?}")),
    }
}

/// Whether a session's guest writes persist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionKind {
    /// Mounts `cards/<card>/`: real, persistent SDMC/NAND reads and writes.
    Primary { card: String },
    /// Private scratch root, empty or copied from `parent`; discarded on end.
    Secondary { parent: Option<String> },
}

/// What a settings read or change addresses (lotus#51).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "target", rename_all = "snake_case")]
pub enum SettingsTarget {
    /// Saved global defaults.
    Global,
    /// A primary card's saved overrides.
    Card { card: String },
    /// A running session: read for any, change for a secondary only.
    Session { session: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingStatus {
    Applied,
    Pending,
    Rejected,
}

#[derive(Debug, Serialize)]
pub struct SettingChange {
    pub key: String,
    pub session: Option<String>,
    pub status: SettingStatus,
    pub reason: Option<String>,
}

/// Identity of a session as listed by every frontend.
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub id: String,
    #[serde(flatten)]
    pub kind: SessionKind,
    /// Approved handle of the loaded content, if any.
    pub asset: Option<String>,
    pub created_unix_ms: u64,
}

impl SessionInfo {
    /// Card a session belongs to in the session tree, if any.
    #[must_use]
    pub fn card(&self) -> Option<&str> {
        match &self.kind {
            SessionKind::Primary { card } => Some(card),
            SessionKind::Secondary { parent } => parent.as_deref(),
        }
    }
}

/// One session row: identity plus latest gauges.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    #[serde(flatten)]
    pub info: SessionInfo,
    pub telemetry: Telemetry,
}

struct SymbolBinding {
    index: lotus_debugger::symbols::Symbols,
    epoch: u64,
    sha256: String,
}

/// A session's lock domain. Lock order: `native`, then `info`, `mailbox`, `telemetry`.
struct SessionHandle {
    info: Mutex<SessionInfo>,
    budgets: Arc<crate::policy::Budgets>,
    root: PathBuf,
    native: tokio::sync::Mutex<NativeSession>,
    telemetry: Mutex<Telemetry>,
    playing: AtomicBool,
    lease: Mutex<Option<String>>,
    symbols: Mutex<Option<SymbolBinding>>,
    /// (epoch, frames) of the last preview served while paused; see [`Supervisor::preview`].
    previewed: Mutex<Option<(u64, u64)>>,
    /// Set once when the session starts stopping; ends its tasks.
    closed: AtomicBool,
    /// Wakes the supervision task early.
    wake: tokio::sync::Notify,
    /// Wakes the input task when the mailbox gains a state.
    input_wake: tokio::sync::Notify,
    mailbox: Mutex<supervision::Mailbox>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Ends the worker process without `native`, for a stop that cannot get the lock.
    kill: Arc<Mutex<crate::controlled_worker::KillSwitch>>,
    /// The worker listed `analog_input`, so queued stick states can be delivered.
    analog_input: bool,
    /// Settings resolved at creation and, for a secondary, its runtime overrides (lotus#51).
    settings: Mutex<SessionSettings>,
}

/// A session's settings: what it started with, and what changed since (secondaries only).
#[derive(Debug, Clone, Default)]
struct SessionSettings {
    /// Resolution at creation; `None` for a checkpoint-seeded session, whose settings come
    /// from the checkpoint.
    snapshot: Option<crate::settings_store::Resolution>,
    /// Secondary runtime overrides; never persisted.
    runtime: BTreeMap<String, serde_json::Value>,
}

impl SessionHandle {
    fn info(&self) -> SessionInfo {
        lock(&self.info).clone()
    }
    fn telemetry(&self) -> MutexGuard<'_, Telemetry> {
        lock(&self.telemetry)
    }
    fn release_tool_budgets(&self) {
        self.budgets.release(&lock(&self.info).id);
    }
    fn record(&self, actor: Actor, operation: &str) {
        self.telemetry().record(actor, operation);
    }
    fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Release);
        self.telemetry().playing = playing;
    }
    fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Acquire)
    }
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    /// Start this session's supervision and input tasks. Requires a Tokio runtime.
    fn start(self: &Arc<Self>) {
        *lock(&self.tasks) = vec![
            tokio::spawn(supervision::supervise(self.clone())),
            tokio::spawn(supervision::deliver_input(self.clone())),
        ];
    }
    /// Mark the session closing; false if it already was.
    fn close(&self) -> bool {
        if self.closed.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.wake.notify_one();
        self.input_wake.notify_one();
        self.cancel_input(true);
        true
    }
    /// Drop pending live input (load, reset, direct input, stop).
    fn cancel_input(&self, forget: bool) {
        let mut mailbox = lock(&self.mailbox);
        let mut telemetry = self.telemetry();
        mailbox.cancel(&mut telemetry.input, forget);
        telemetry.input.pending = 0;
    }
}

#[derive(Default)]
struct Registry {
    sessions: BTreeMap<String, Arc<SessionHandle>>,
    /// Creations in progress, counted against the session limit.
    pending: usize,
    /// Card name to mounting session (or pending creation) ID.
    mounted: BTreeMap<String, String>,
    /// Cards being copied while no primary holds them; a primary waits for the copy.
    cloning: BTreeMap<String, usize>,
}

/// No shell execution, arbitrary caller paths or remote listener.
pub struct Supervisor {
    config: HostConfig,
    cards: CardStore,
    registry: Mutex<Registry>,
    artifacts: Mutex<ArtifactStore>,
    /// Content approved interactively by the desktop user.
    local_assets: Mutex<BTreeMap<String, PathBuf>>,
    focus_request: Mutex<Option<String>>,
    /// Session holding exclusive audio focus, by the latest request.
    audio_focus: Mutex<Option<String>>,
    /// Set by [`Supervisor::close`]; refuses new sessions.
    closing: AtomicBool,
    /// Loop cadence and latency reported by a windowed frontend; null when headless.
    frontend_metrics: Mutex<serde_json::Value>,
    /// Serializes shader seed publication between concurrent clean stops.
    shader_publish: tokio::sync::Mutex<()>,
    /// Holders of the expensive-tool budgets and the snapshot slots.
    budgets: Arc<crate::policy::Budgets>,
    /// Session whose worker runs at raised priority (`policy.focus_priority`).
    focused: Mutex<Option<String>>,
    /// Saved global and card settings (lotus#51).
    settings: crate::settings_store::SettingsStore,
    /// Keep saves and subsequent live updates in the same order within this host.
    settings_edits: tokio::sync::Mutex<()>,
    /// Bumped by every settings or input-preset change, so a frontend knows when to
    /// re-resolve input layouts (lotus#63).
    settings_revision: AtomicU64,
    /// Window layout (lotus#67). Lock order: `registry`, then `workspace`.
    workspace: Mutex<crate::workspace::Workspace>,
    workspace_resolved: Mutex<Option<crate::workspace::Resolved>>,
    /// Set by a windowed frontend; headless hosts refuse workspace changes.
    workspace_available: AtomicBool,
}

impl Supervisor {
    pub fn new(mut config: HostConfig) -> io::Result<Self> {
        if !(1..=MAX_SESSIONS_LIMIT).contains(&config.max_sessions) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("max_sessions must be in 1..={MAX_SESSIONS_LIMIT}"),
            ));
        }
        validate_profile(&config.profile)?;
        config.worker = canonical("worker", &config.worker)?;
        for (name, worker) in &mut config.workers {
            *worker = canonical(&format!("workers[\"{name}\"]"), worker)?;
        }
        for (handle, path) in &mut config.assets {
            *path = canonical(&format!("assets[\"{handle}\"]"), path)?;
        }
        for (handle, path) in &mut config.asset_directories {
            if handle.is_empty() || handle.contains(['/', '\\']) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "asset directory handles cannot be empty or contain separators",
                ));
            }
            // A directory may be configured before it exists (a build output folder). It
            // lists nothing until created and is canonicalized again on every use.
            *path = std::path::absolute(&*path).map_err(|error| {
                labeled(&format!("asset_directories[\"{handle}\"]"), path, error)
            })?;
        }
        fs::create_dir_all(&config.storage_root)
            .map_err(|error| labeled("storage_root", &config.storage_root, error))?;
        config.storage_root = canonical("storage_root", &config.storage_root)?;
        let cards = CardStore::open(&config.storage_root)?;
        let artifacts = ArtifactStore::open(
            config.storage_root.join("artifacts"),
            config.artifacts.clone(),
            config.max_sessions,
        )?;
        let policy = config.policy.clone();
        let settings =
            crate::settings_store::SettingsStore::new(&config.storage_root, &config.profile)?;
        Ok(Self {
            settings,
            config,
            cards,
            registry: Mutex::default(),
            artifacts: Mutex::new(artifacts),
            local_assets: Mutex::default(),
            focus_request: Mutex::default(),
            audio_focus: Mutex::default(),
            closing: AtomicBool::new(false),
            frontend_metrics: Mutex::default(),
            shader_publish: tokio::sync::Mutex::default(),
            budgets: Arc::new(crate::policy::Budgets::new(policy)),
            focused: Mutex::default(),
            workspace: Mutex::default(),
            workspace_resolved: Mutex::default(),
            workspace_available: AtomicBool::new(false),
            settings_edits: tokio::sync::Mutex::new(()),
            settings_revision: AtomicU64::new(0),
        })
    }

    #[must_use]
    pub fn max_sessions(&self) -> usize {
        self.config.max_sessions
    }

    #[must_use]
    pub fn allows_control(&self) -> bool {
        self.config.allow_control
    }

    pub fn require_control(&self) -> io::Result<()> {
        if self.config.allow_control {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "host has not enabled control for this client",
            ))
        }
    }

    /// Session IDs in creation-independent sorted order.
    #[must_use]
    pub fn sessions(&self) -> Vec<String> {
        lock(&self.registry).sessions.keys().cloned().collect()
    }

    /// Every session with its latest gauges; never waits on a worker.
    #[must_use]
    pub fn summaries(&self) -> Vec<SessionSummary> {
        self.handles()
            .iter()
            .map(|handle| SessionSummary {
                info: handle.info(),
                telemetry: handle.telemetry().clone(),
            })
            .collect()
    }

    pub fn summary(&self, id: &str) -> io::Result<SessionSummary> {
        let handle = self.handle(id)?;
        let telemetry = handle.telemetry().clone();
        Ok(SessionSummary {
            info: handle.info(),
            telemetry,
        })
    }

    /// Check a debugger target against the latest published worker state without
    /// waiting for its IPC lock. Scoped dispatch also checks the live epoch.
    pub fn debug_target_current(&self, id: &str, expected: (u64, u64)) -> bool {
        !self.is_closing()
            && self.handle(id).is_ok_and(|handle| {
                !handle.is_closed() && handle.info().created_unix_ms == expected.0 && {
                    let telemetry = handle.telemetry();
                    telemetry.state.is_active() && telemetry.epoch == expected.1
                }
            })
    }

    /// Persistent card names.
    /// The host configuration this supervisor runs with (paths canonicalized).
    #[must_use]
    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    pub fn cards(&self) -> io::Result<Vec<String>> {
        self.cards.list()
    }

    /// Cards currently mounted by a primary session of this host.
    #[must_use]
    pub fn mounted_cards(&self) -> BTreeMap<String, String> {
        lock(&self.registry).mounted.clone()
    }

    /// Approved content handles: configured, directory-listed and desktop-approved.
    #[must_use]
    pub fn assets(&self) -> Vec<String> {
        let mut handles: Vec<String> = self.config.assets.keys().cloned().collect();
        for (handle, directory) in &self.config.asset_directories {
            let Ok(entries) = directory.canonicalize().and_then(fs::read_dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_content_name(&name) && entry.path().is_file() {
                    handles.push(format!("{handle}/{name}"));
                }
            }
        }
        handles.extend(lock(&self.local_assets).keys().cloned());
        handles.sort();
        handles.dedup();
        handles
    }

    /// Configured asset directories that cannot be listed now, with the reason.
    #[must_use]
    pub fn unavailable_asset_directories(&self) -> BTreeMap<String, String> {
        self.config
            .asset_directories
            .iter()
            .filter_map(|(handle, directory)| {
                let error = match directory.canonicalize() {
                    Ok(path) if path.is_dir() => return None,
                    Ok(_) => "not a directory".to_string(),
                    Err(error) => error.to_string(),
                };
                Some((handle.clone(), format!("{}: {error}", directory.display())))
            })
            .collect()
    }

    /// Full local failure report of a session's worker, including log tails. For the local
    /// desktop user only; never exposed through MCP. Waits for the session lock.
    pub async fn failure_report(&self, id: &str) -> io::Result<Option<String>> {
        let handle = self.handle(id)?;
        let native = handle.native.lock().await;
        native
            .failure()
            .map(|report| serde_json::to_string_pretty(report).map_err(io::Error::other))
            .transpose()
    }

    /// Approve a file chosen by the local desktop user. Never reachable from MCP.
    pub fn approve_local_asset(&self, path: &Path) -> io::Result<String> {
        let path = path.canonicalize()?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| is_content_name(name))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "content must be a .3dsx file")
            })?;
        let mut assets = lock(&self.local_assets);
        let known = assets
            .iter()
            .find(|(_, known)| **known == path)
            .map(|(handle, _)| handle.clone());
        if let Some(handle) = known {
            return Ok(handle);
        }
        let mut handle = format!("local/{name}");
        let mut suffix = 2;
        while assets.contains_key(&handle) || self.config.assets.contains_key(&handle) {
            handle = format!("local/{suffix}-{name}");
            suffix += 1;
        }
        assets.insert(handle.clone(), path);
        Ok(handle)
    }

    /// Spawn a worker for `kind`, optionally loading approved content paused.
    pub async fn create(
        &self,
        kind: SessionKind,
        asset: Option<&str>,
        actor: Actor,
    ) -> io::Result<SessionInfo> {
        self.create_seeded(kind, asset, actor, None, None).await
    }

    async fn create_seeded(
        &self,
        kind: SessionKind,
        asset: Option<&str>,
        actor: Actor,
        seed: Option<(
            &Path,
            &BTreeMap<String, lotus_protocol::debug::SettingValue>,
        )>,
        worker: Option<&Path>,
    ) -> io::Result<SessionInfo> {
        self.require_control()?;
        let content = asset.map(|handle| self.resolve_asset(handle)).transpose()?;
        let id = SessionUuid::generate().to_string();
        self.host_memory().admit()?;
        let gpu_refusal = self.gpu_memory().refusal();
        let reservation = self.reserve(&id, &kind)?;
        let root = match &kind {
            SessionKind::Primary { card } => self.cards.primary_root(card)?,
            SessionKind::Secondary { parent: None } => self.cards.create_scratch(&id, None)?,
            SessionKind::Secondary { parent: Some(card) } => {
                let _slot = self.budgets.snapshot("card copy").await?;
                self.clone_card(&id, card).await?
            }
        };
        // Global, then the card's overrides (a primary's own card, a secondary's parent),
        // then the host profile. A secondary keeps this snapshot for its lifetime (lotus#51).
        let resolved = match seed {
            Some(_) => None,
            None => {
                let card = match &kind {
                    SessionKind::Primary { card } => Some(card.as_str()),
                    SessionKind::Secondary { parent } => parent.as_deref(),
                };
                match self.settings.resolve(card) {
                    Ok(resolution) => Some(resolution),
                    Err(error) => {
                        if matches!(kind, SessionKind::Secondary { .. }) {
                            let _ = self.cards.discard_scratch(&root, ScratchEnd::Abnormal);
                        }
                        return Err(error);
                    }
                }
            }
        };
        let resolved_profile = resolved
            .as_ref()
            .map(crate::settings_store::Resolution::profile);
        let launch = Instant::now();
        let executable = worker.unwrap_or(&self.config.worker);
        let mut shader_state = "off";
        let started = async {
            if let Some((source, _)) = seed {
                crate::card_store::copy_checkpoint_root(source, &root)?;
            }
            if self.config.shader_seed {
                shader_state = self.seed_shaders(executable, &root).await?;
            }
            let mut native = NativeSession::spawn(executable, &root).await?;
            let profile = match (seed, &resolved_profile) {
                (Some((_, profile)), _) => profile,
                (None, Some(profile)) => profile,
                (None, None) => &self.config.profile,
            };
            // An explicit Vulkan profile is refused under GPU pressure; Vulkan only preferred
            // starts on software before load, the restartable boundary prefer_vulkan already
            // falls back at (lotus#46).
            let explicit_vulkan = matches!(
                profile.get("graphics_api"),
                Some(lotus_protocol::debug::SettingValue::Integer(2))
            );
            if let (true, Some(refusal)) = (explicit_vulkan, &gpu_refusal) {
                let _ = native.stop().await;
                return Err(io::Error::new(io::ErrorKind::OutOfMemory, refusal.clone()));
            }
            if let Err(error) = native.apply_profile(profile).await {
                let _ = native.stop().await;
                return Err(error);
            }
            if seed.is_none()
                && self.config.prefer_vulkan
                && native.has_settings()
                && !profile.contains_key("graphics_api")
            {
                let chosen = match &gpu_refusal {
                    Some(refusal) => native.start_on_software(refusal.clone()).await,
                    None => native.prefer_vulkan().await,
                };
                if let Err(error) = chosen {
                    let _ = native.stop().await;
                    return Err(error);
                }
            }
            if let Some(content) = &content {
                if let Err(error) = native.load(content).await {
                    let _ = native.stop().await;
                    return Err(error);
                }
            }
            Ok(native)
        }
        .await;
        let native = match started {
            Ok(native) => native,
            Err(error) => {
                if matches!(kind, SessionKind::Secondary { .. }) {
                    let _ = self.cards.discard_scratch(&root, ScratchEnd::Abnormal);
                }
                return Err(error);
            }
        };
        let info = SessionInfo {
            id: id.clone(),
            kind,
            asset: asset.map(str::to_string),
            created_unix_ms: unix_ms(),
        };
        let mut telemetry = Telemetry::new();
        telemetry.state = native.state();
        telemetry.epoch = native.epoch();
        telemetry.backend_name = native.backend_name().to_owned();
        telemetry.pid = native.pid();
        telemetry.worker_features = Arc::new(native.features().clone());
        telemetry.startup_ms = launch.elapsed().as_secs_f64() * 1e3;
        telemetry.shader_seed = shader_state.to_string();
        telemetry.record(actor, "create");
        let kill = native.kill_switch();
        let analog_input = native
            .features()
            .contains(lotus_protocol::capabilities::feature::ANALOG_INPUT);
        let handle = Arc::new(SessionHandle {
            info: Mutex::new(info.clone()),
            budgets: self.budgets.clone(),
            root,
            native: tokio::sync::Mutex::new(native),
            telemetry: Mutex::new(telemetry),
            playing: AtomicBool::new(false),
            lease: Mutex::new(None),
            symbols: Mutex::new(None),
            previewed: Mutex::new(None),
            closed: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
            input_wake: tokio::sync::Notify::new(),
            mailbox: Mutex::default(),
            tasks: Mutex::default(),
            kill,
            analog_input,
            settings: Mutex::new(SessionSettings {
                snapshot: resolved,
                runtime: BTreeMap::new(),
            }),
        });
        handle.start();
        reservation.commit(handle);
        self.settings_revision.fetch_add(1, Ordering::AcqRel);
        Ok(info)
    }

    /// Load approved content, replacing what was loaded. Leaves the session paused.
    pub async fn load(&self, id: &str, asset: &str, actor: Actor) -> io::Result<SessionInfo> {
        self.require_control()?;
        let content = self.resolve_asset(asset)?;
        let handle = self.handle(id)?;
        let mut native = handle.native.lock().await;
        check_lease(&handle, actor_client(actor))?;
        handle.set_playing(false);
        pause_if_running(&mut native).await?;
        handle.cancel_input(true);
        native.load(&content).await?;
        handle.telemetry().epoch = native.epoch();
        self.budgets.clear_recordings(id);
        lock(&handle.info).asset = Some(asset.to_string());
        self.settings_revision.fetch_add(1, Ordering::AcqRel);
        handle.record(actor, "load");
        Ok(handle.info())
    }

    fn known_card(&self, card: &str) -> io::Result<()> {
        crate::card_store::validate_name(card)?;
        if self.cards.list()?.iter().any(|known| known == card) {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("unknown card '{card}'"),
        ))
    }

    /// Saved settings for a global/card target and observed settings for a session.
    /// A mounted card also reports its worker's effective values and pending changes.
    pub async fn settings(&self, target: &SettingsTarget) -> io::Result<serde_json::Value> {
        use crate::settings_store::Resolution;
        let report = |resolution: &Resolution, extra: serde_json::Value| {
            let mut value = serde_json::to_value(resolution).map_err(io::Error::other)?;
            if let (Some(value), serde_json::Value::Object(extra)) = (value.as_object_mut(), extra)
            {
                value.extend(extra);
            }
            value["managed"] =
                serde_json::to_value(self.settings.managed().keys().collect::<Vec<_>>())
                    .map_err(io::Error::other)?;
            Ok(value)
        };
        match target {
            SettingsTarget::Global => {
                let desired = self.settings.resolve(None)?;
                let mut pending: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
                for id in self.sessions() {
                    let Ok(handle) = self.handle(&id) else {
                        continue;
                    };
                    let SessionKind::Primary { card } = handle.info().kind else {
                        continue;
                    };
                    let card_settings = self.settings.resolve(Some(&card))?;
                    let (_, view) = self.session_settings(&handle, &card_settings, true).await?;
                    if let Some(changes) = view["pending_details"].as_object() {
                        for (key, entry) in changes {
                            if card_settings.settings[key].source
                                != crate::settings_store::Layer::Card
                            {
                                let mut entry = entry.clone();
                                entry["card"] = card.clone().into();
                                pending.entry(key.clone()).or_default().push(entry);
                            }
                        }
                    }
                }
                report(&desired, serde_json::json!({ "pending_cards": pending }))
            }
            SettingsTarget::Card { card } => {
                self.known_card(card)?;
                let desired = self.settings.resolve(Some(card))?;
                let handle = {
                    let registry = lock(&self.registry);
                    registry
                        .mounted
                        .get(card)
                        .and_then(|id| registry.sessions.get(id))
                        .cloned()
                };
                let mut extra = serde_json::json!({ "mounted_by": null });
                if let Some(handle) = handle {
                    let (effective, mut session) =
                        self.session_settings(&handle, &desired, true).await?;
                    session["mounted_by"] = handle.info().id.into();
                    session["effective_settings"] =
                        serde_json::to_value(effective.settings).map_err(io::Error::other)?;
                    extra = session;
                }
                report(&desired, extra)
            }
            SettingsTarget::Session { session } => {
                let handle = self.handle(session)?;
                let info = handle.info();
                let saved = self.settings.resolve(info.card())?;
                let primary = matches!(info.kind, SessionKind::Primary { .. });
                let (effective, mut extra) =
                    self.session_settings(&handle, &saved, primary).await?;
                extra["session"] = serde_json::to_value(info).map_err(io::Error::other)?;
                report(&effective, extra)
            }
        }
    }

    async fn session_settings(
        &self,
        handle: &SessionHandle,
        saved: &crate::settings_store::Resolution,
        primary: bool,
    ) -> io::Result<(crate::settings_store::Resolution, serde_json::Value)> {
        use crate::settings_store::Layer;
        use lotus_settings::{Apply, Registry};

        let native = handle.native.lock().await;
        let session = lock(&handle.settings).clone();
        let mut effective = session.snapshot.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "this session was restored from a checkpoint and runs with its settings",
            )
        })?;
        let mut desired = if primary {
            saved.clone()
        } else {
            effective.clone()
        };
        // Input settings resolve from the current saved layers on every revision.
        for (key, entry) in &saved.settings {
            if is_input(key) {
                effective.settings.insert(key.clone(), entry.clone());
                desired.settings.insert(key.clone(), entry.clone());
            }
        }
        effective.apply_runtime(&session.runtime);
        desired.apply_runtime(&session.runtime);
        let available = native.state().is_active() && !handle.is_closed();
        let mut pending = BTreeMap::new();
        effective.settings.retain(|key, entry| {
            if is_input(key) {
                return true;
            }
            let observed = available
                .then(|| native.settings_values().get(key))
                .flatten();
            if let Some(next) = desired.settings.get(key).filter(|_| primary) {
                let class = Registry::builtin().get(key).map(|d| d.apply);
                let next_value = self.next_setting_value(&native, key, next);
                let changed = !same_setting(key, &entry.value, &next.value)
                    || (entry.source == Layer::Default) != (next.source == Layer::Default)
                    || (class == Some(Apply::CfgSavedata) && entry.source == Layer::Default);
                let inherit_cfg =
                    class == Some(Apply::CfgSavedata) && next.source == Layer::Default;
                if changed
                    && !inherit_cfg
                    && !observed.is_some_and(|v| same_setting(key, v, next_value))
                {
                    let reason = match class {
                        Some(Apply::CfgSavedata) => {
                            "Updated in guest CFG savedata before the next start."
                        }
                        _ => "The saved value applies the next time this card starts.",
                    };
                    pending.insert(
                        key.clone(),
                        serde_json::json!({
                            "status": "pending",
                            "apply": class,
                            "effective": observed,
                            "desired": next_value,
                            "next_action": "start_card",
                            "reason": reason,
                        }),
                    );
                }
            }
            if let Some(observed) = observed {
                if !same_setting(key, observed, &entry.value) {
                    entry.source = Layer::Worker;
                }
                entry.value = observed.clone();
                true
            } else {
                false
            }
        });
        let pending_keys: Vec<_> = pending.keys().collect();
        let runtime: Vec<_> = session.runtime.keys().collect();
        Ok((
            effective,
            serde_json::json!({
                "desired_settings": desired.settings,
                "pending": pending_keys,
                "pending_details": pending,
                "runtime": runtime,
            }),
        ))
    }

    fn next_setting_value<'a>(
        &self,
        native: &'a NativeSession,
        key: &str,
        entry: &'a crate::settings_store::Entry,
    ) -> &'a serde_json::Value {
        if entry.source == crate::settings_store::Layer::Default
            && !(key == "graphics_api" && self.config.prefer_vulkan)
        {
            return native.startup_defaults().get(key).unwrap_or(&entry.value);
        }
        &entry.value
    }

    /// Save or clear one override. Live changes reach the affected mounted primary.
    /// A secondary override is local to that session.
    pub async fn set_setting(
        &self,
        target: &SettingsTarget,
        key: &str,
        value: Option<serde_json::Value>,
        actor: Actor,
    ) -> io::Result<Vec<SettingChange>> {
        use crate::settings_store::Store;
        self.require_control()?;
        let result = match target {
            SettingsTarget::Global => {
                self.change_saved(&Store::Global, Some((key, value)), actor)
                    .await
            }
            SettingsTarget::Card { card } => {
                self.known_card(card)?;
                self.change_saved(&Store::Card(card.clone()), Some((key, value)), actor)
                    .await
            }
            SettingsTarget::Session { session } => self
                .set_runtime(session, [(key.to_string(), value)], actor)
                .await
                .map(|()| {
                    vec![SettingChange {
                        key: key.into(),
                        session: Some(session.clone()),
                        status: SettingStatus::Applied,
                        reason: None,
                    }]
                }),
        };
        self.settings_revision.fetch_add(1, Ordering::AcqRel);
        result
    }

    /// Remove every override of a target: a saved file's entries, or a secondary's runtime
    /// overrides (re-applying its creation values).
    pub async fn reset_settings(
        &self,
        target: &SettingsTarget,
        actor: Actor,
    ) -> io::Result<Vec<SettingChange>> {
        use crate::settings_store::Store;
        self.require_control()?;
        let result = match target {
            SettingsTarget::Global => self.change_saved(&Store::Global, None, actor).await,
            SettingsTarget::Card { card } => {
                self.known_card(card)?;
                self.change_saved(&Store::Card(card.clone()), None, actor)
                    .await
            }
            SettingsTarget::Session { session } => {
                let handle = self.handle(session)?;
                let keys: Vec<String> = lock(&handle.settings).runtime.keys().cloned().collect();
                self.set_runtime(session, keys.iter().cloned().map(|key| (key, None)), actor)
                    .await
                    .map(|()| {
                        keys.into_iter()
                            .map(|key| SettingChange {
                                key,
                                session: Some(session.clone()),
                                status: SettingStatus::Applied,
                                reason: None,
                            })
                            .collect()
                    })
            }
        };
        self.settings_revision.fetch_add(1, Ordering::AcqRel);
        result
    }

    async fn change_saved(
        &self,
        store: &crate::settings_store::Store,
        edit: Option<(&str, Option<serde_json::Value>)>,
        actor: Actor,
    ) -> io::Result<Vec<SettingChange>> {
        use crate::settings_store::{check, Layer, Store};
        use lotus_settings::{Apply, Registry, Scope};

        let _edit_guard = self.settings_edits.lock().await;
        let card = match store {
            Store::Global => None,
            Store::Card(card) => Some(card.as_str()),
        };
        let previous = self.settings.resolve(card)?;
        let mounted: Vec<_> = lock(&self.registry)
            .sessions
            .values()
            .filter_map(|handle| {
                let SessionKind::Primary { card: name } = handle.info().kind else {
                    return None;
                };
                (card.is_none_or(|card| card == name) && !handle.is_closed())
                    .then(|| (name, Arc::clone(handle)))
            })
            .collect();
        let mut affected = Vec::with_capacity(mounted.len());
        for (name, handle) in mounted {
            let before = self.settings.resolve(Some(&name))?;
            if let Some((key, Some(value))) = edit.as_ref().filter(|(key, _)| {
                !is_input(key)
                    && !(card.is_none()
                        && before.settings.get(*key).is_some_and(|entry| {
                            matches!(entry.source, Layer::Card | Layer::Managed)
                        }))
            }) {
                let native = handle.native.lock().await;
                check(
                    key,
                    value,
                    if card.is_some() {
                        Scope::Primary
                    } else {
                        Scope::Global
                    },
                    native.branch(),
                )?;
                if native.state().is_active()
                    && native.branch().is_some()
                    && !native.settings_values().contains_key(*key)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        format!("mounted worker does not report {key}"),
                    ));
                }
            }
            affected.push((name, handle, before));
        }
        match &edit {
            Some((key, value)) => self.settings.set(store, key, value.clone())?,
            None => self.settings.reset(store)?,
        }

        let saved = self.settings.resolve(card)?;
        let affected = affected
            .into_iter()
            .map(|(name, handle, before)| Ok((handle, before, self.settings.resolve(Some(&name))?)))
            .collect::<io::Result<Vec<_>>>()?;
        let keys: Vec<&str> = match &edit {
            Some((key, _)) => vec![key],
            None => saved
                .settings
                .iter()
                .filter(|(key, entry)| previous.settings.get(*key) != Some(*entry))
                .map(|(key, _)| key.as_str())
                .collect(),
        };
        let mut changes = Vec::new();
        for key in keys {
            let Some(descriptor) = Registry::builtin().get(key) else {
                continue;
            };
            let mut matched = false;
            for (handle, before, after) in &affected {
                let Some(next) = after.settings.get(key) else {
                    continue;
                };
                if before.settings.get(key) == Some(next)
                    && matches!(next.source, Layer::Card | Layer::Managed)
                    && card.is_none()
                {
                    continue;
                }
                matched = true;
                let (status, reason) = match self
                    .apply_saved_setting(handle, key, next, descriptor.apply, actor)
                    .await
                {
                    Ok(SettingStatus::Pending) => (
                        SettingStatus::Pending,
                        Some("Saved for the next card start.".into()),
                    ),
                    Ok(status) => (status, None),
                    Err(error) => (SettingStatus::Rejected, Some(error.to_string())),
                };
                changes.push(SettingChange {
                    key: key.into(),
                    session: Some(handle.info().id),
                    status,
                    reason,
                });
            }
            if !matched {
                changes.push(SettingChange {
                    key: key.into(),
                    session: None,
                    status: if descriptor.apply == Apply::Routing {
                        SettingStatus::Applied
                    } else {
                        SettingStatus::Pending
                    },
                    reason: (descriptor.apply != Apply::Routing)
                        .then(|| "Saved for future session starts.".into()),
                });
            }
        }
        Ok(changes)
    }

    async fn apply_saved_setting(
        &self,
        handle: &SessionHandle,
        key: &str,
        next: &crate::settings_store::Entry,
        apply: lotus_settings::Apply,
        actor: Actor,
    ) -> io::Result<SettingStatus> {
        use crate::settings_store::Layer;
        use lotus_settings::Apply;

        if apply == Apply::Routing || (apply == Apply::CfgSavedata && next.source == Layer::Default)
        {
            return Ok(SettingStatus::Applied);
        }
        let mut native = handle.native.lock().await;
        if handle.is_closed() || !native.state().is_active() {
            return Ok(SettingStatus::Pending);
        }
        let requested = self.next_setting_value(&native, key, next).clone();
        let matches_worker = |native: &NativeSession| {
            native
                .settings_values()
                .get(key)
                .is_some_and(|value| same_setting(key, value, &requested))
        };
        if apply != Apply::Live {
            return Ok(if matches_worker(&native) {
                SettingStatus::Applied
            } else {
                SettingStatus::Pending
            });
        }
        if !matches_worker(&native) {
            check_lease(handle, actor_client(actor))?;
            let value = serde_json::from_value(requested.clone()).map_err(io::Error::other)?;
            native
                .apply_profile(&BTreeMap::from([(key.to_string(), value)]))
                .await?;
            if !matches_worker(&native) {
                return Err(io::Error::other(
                    "Worker reported a different effective value.",
                ));
            }
        }
        if let Some(snapshot) = &mut lock(&handle.settings).snapshot {
            snapshot.settings.insert(key.into(), next.clone());
        }
        handle.record(actor, "settings");
        Ok(SettingStatus::Applied)
    }

    /// Changes so far to settings and input presets; see [`Supervisor::input_layout`].
    #[must_use]
    pub fn settings_revision(&self) -> u64 {
        self.settings_revision.load(Ordering::Acquire)
    }

    /// The input layout `session` uses now (lotus#63). Unlike worker settings it is resolved
    /// live, so saved global and card edits reach running sessions: global, then the card
    /// (a primary's own, a secondary's parent), then a secondary's runtime overrides.
    pub fn input_layout(
        &self,
        session: &str,
    ) -> io::Result<(lotus_input::layout::Resolved, lotus_input::layout::Profile)> {
        let handle = self.handle(session)?;
        let card = match handle.info().kind {
            SessionKind::Primary { card } => Some(card),
            SessionKind::Secondary { parent } => parent,
        };
        let mut resolution = self.settings.resolve(card.as_deref())?;
        resolution.apply_runtime(&lock(&handle.settings).runtime);
        let input = resolution.input();
        let (library, _) = self.settings.input_presets();
        let mut resolved =
            lotus_input::layout::resolve(&library, input.preset.as_deref(), &input.overrides);
        resolved.problems.extend(input.problems);
        Ok((resolved, input.profile))
    }

    /// Saved input presets and files that could not be read.
    #[must_use]
    pub fn input_presets(&self) -> (Vec<lotus_input::layout::Preset>, Vec<String>) {
        self.settings.input_presets()
    }

    /// Export a builtin or saved input preset as JSON.
    pub fn export_input_preset(&self, name: &str) -> io::Result<String> {
        self.settings.export_preset(name)
    }

    /// Create, import, duplicate, rename or delete a saved input preset; returns its name
    /// and what an import or save dropped or filled in (unknown actions and bindings,
    /// actions taking the builtin bindings).
    pub fn change_input_preset(
        &self,
        change: crate::settings_store::PresetChange,
    ) -> io::Result<(String, Vec<lotus_input::layout::Problem>)> {
        self.require_control()?;
        let result = self.settings.change_preset(change);
        self.settings_revision.fetch_add(1, Ordering::AcqRel);
        result
    }

    async fn set_runtime(
        &self,
        session: &str,
        changes: impl IntoIterator<Item = (String, Option<serde_json::Value>)>,
        actor: Actor,
    ) -> io::Result<()> {
        let handle = self.handle(session)?;
        if matches!(handle.info().kind, SessionKind::Primary { .. }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a primary session's settings are its card's overrides: set them on the card",
            ));
        }
        let mut native = handle.native.lock().await;
        check_lease(&handle, actor_client(actor))?;
        for (key, value) in changes {
            let snapshot = lock(&handle.settings).snapshot.clone().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "this session was restored from a checkpoint and runs with its settings",
                )
            })?;
            if self.settings.managed().contains_key(&key) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("{key} is managed by the host configuration profile"),
                ));
            }
            if is_input(&key) {
                // Input layout: routed by the frontend, never sent to the worker.
                if let Some(value) = &value {
                    crate::settings_store::check(
                        &key,
                        value,
                        lotus_settings::Scope::SecondaryRuntime,
                        native.branch(),
                    )?;
                }
                let mut settings = lock(&handle.settings);
                match value {
                    Some(value) => settings.runtime.insert(key, value),
                    None => settings.runtime.remove(&key),
                };
                continue;
            }
            let applied = match &value {
                Some(value) => {
                    crate::settings_store::check(
                        &key,
                        value,
                        lotus_settings::Scope::SecondaryRuntime,
                        native.branch(),
                    )?;
                    value.clone()
                }
                None => snapshot
                    .settings
                    .get(&key)
                    .map(|entry| entry.value.clone())
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            format!("unknown setting {key}"),
                        )
                    })?,
            };
            let setting: lotus_protocol::debug::SettingValue =
                serde_json::from_value(applied).map_err(io::Error::other)?;
            // The worker refuses a non-live class while powered on, naming the class.
            native
                .apply_profile(&BTreeMap::from([(key.clone(), setting)]))
                .await?;
            let mut settings = lock(&handle.settings);
            match value {
                Some(value) => settings.runtime.insert(key, value),
                None => settings.runtime.remove(&key),
            };
        }
        handle.record(actor, "settings");
        Ok(())
    }

    /// Run continuously: the session's supervision task re-issues bounded runs until paused.
    pub async fn run(&self, id: &str, actor: Actor) -> io::Result<()> {
        self.require_control()?;
        let handle = self.handle(id)?;
        if handle.info().asset.is_none() {
            return Err(io::Error::other("load content before running"));
        }
        let mut native = handle.native.lock().await;
        check_lease(&handle, actor_client(actor))?;
        if native.state() == SessionState::Paused {
            native.run().await?;
        }
        handle.set_playing(true);
        handle.record(actor, "run");
        Ok(())
    }

    pub async fn pause(&self, id: &str, actor: Actor) -> io::Result<serde_json::Value> {
        self.require_control()?;
        let handle = self.handle(id)?;
        let mut native = handle.native.lock().await;
        check_lease(&handle, actor_client(actor))?;
        handle.set_playing(false);
        handle.record(actor, "pause");
        native.pause().await
    }

    /// Advance exactly `frames` (1–600) and stay paused.
    pub async fn advance(
        &self,
        id: &str,
        frames: u16,
        actor: Actor,
    ) -> io::Result<serde_json::Value> {
        self.require_control()?;
        let handle = self.handle(id)?;
        let mut native = handle.native.lock().await;
        check_lease(&handle, actor_client(actor))?;
        handle.set_playing(false);
        pause_if_running(&mut native).await?;
        handle.record(actor, &format!("run_until {frames}"));
        native.advance(frames).await
    }

    pub async fn reset(&self, id: &str, actor: Actor) -> io::Result<serde_json::Value> {
        self.require_control()?;
        let handle = self.handle(id)?;
        let mut native = handle.native.lock().await;
        check_lease(&handle, actor_client(actor))?;
        handle.set_playing(false);
        pause_if_running(&mut native).await?;
        handle.record(actor, "reset");
        handle.cancel_input(true);
        let result = native.reset().await?;
        handle.telemetry().epoch = native.epoch();
        self.budgets.clear_recordings(id);
        self.settings_revision.fetch_add(1, Ordering::AcqRel);
        Ok(result)
    }

    pub async fn input(
        &self,
        id: &str,
        state: InputState,
        owner: &str,
        actor: Actor,
    ) -> io::Result<serde_json::Value> {
        self.require_control()?;
        let handle = self.handle(id)?;
        let mut native = handle.native.lock().await;
        check_lease(&handle, actor_client(actor))?;
        if actor != Actor::Desktop {
            handle.record(actor, "input");
        }
        // A direct state supersedes anything still queued for this session.
        handle.cancel_input(true);
        native.input(state, owner).await
    }

    /// Queue a complete live input state and return at once. The session's own input task
    /// delivers the newest state after the previous exchange, keeping short press/release
    /// transitions ([`supervision::Mailbox`]). Errors surface in telemetry, not here.
    pub fn submit_input(
        &self,
        id: &str,
        state: InputState,
        owner: &str,
        actor: Actor,
    ) -> io::Result<()> {
        self.require_control()?;
        state.validate()?;
        let handle = self.handle(id)?;
        if handle.is_closed() {
            return Err(io::Error::other("session is stopping"));
        }
        // Refused here, not in the delivery task, so the caller learns it (lotus#60).
        if state.analog() && !handle.analog_input {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this worker does not list analog_input; stick input would be ignored",
            ));
        }
        {
            let mut mailbox = lock(&handle.mailbox);
            let mut telemetry = handle.telemetry();
            mailbox.submit(
                supervision::Queued {
                    state,
                    owner: owner.to_string(),
                    actor,
                    submitted: Instant::now(),
                },
                &mut telemetry.input,
            );
            telemetry.input.pending = mailbox.len();
        }
        handle.input_wake.notify_one();
        Ok(())
    }

    /// End a session. Secondary guest data is deleted; primary data persists.
    ///
    /// Bounded by [`CLOSE_DEADLINE`]: if a stalled exchange or a slow command still holds the
    /// session then, its worker's job is terminated through the kill switch, which fails the
    /// exchange and releases the session. The result is then `TimedOut`.
    pub async fn stop(&self, id: &str, actor: Actor) -> io::Result<()> {
        self.require_control()?;
        self.stop_session(id, actor).await
    }

    async fn stop_session(&self, id: &str, actor: Actor) -> io::Result<()> {
        let handle = self.handle(id)?;
        if !handle.close() {
            return Err(io::Error::other("session is already stopping"));
        }
        self.stop_claimed(id, actor, handle).await
    }

    async fn stop_claimed(
        &self,
        id: &str,
        actor: Actor,
        handle: Arc<SessionHandle>,
    ) -> io::Result<()> {
        handle.set_playing(false);
        handle.record(actor, "stop");
        let started = Instant::now();
        let terminated =
            || io::Error::new(io::ErrorKind::TimedOut, "worker stop deadline; terminated");
        let mut forced = false;
        let mut native = match tokio::time::timeout(CLOSE_DEADLINE, handle.native.lock()).await {
            Ok(native) => native,
            Err(_) => {
                forced = lock(&handle.kill).terminate();
                match tokio::time::timeout(RELEASE_DEADLINE, handle.native.lock()).await {
                    Ok(native) => native,
                    Err(_) => {
                        // A host-side operation (not an exchange) still holds the session.
                        // Its worker is gone; the handle and root lock go when it returns.
                        self.unregister(id);
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "session held by a host operation; worker terminated, cleanup deferred",
                        ));
                    }
                }
            }
        };
        let remaining = CLOSE_DEADLINE
            .saturating_sub(started.elapsed())
            .max(Duration::from_millis(250));
        let result = if native.state().is_active() {
            match tokio::time::timeout(remaining, native.stop()).await {
                Ok(result) => result,
                Err(_) => {
                    native.abort().await;
                    Err(terminated())
                }
            }
        } else if forced {
            Err(terminated())
        } else {
            Ok(())
        };
        let end = if result.is_ok() && native.state() == SessionState::Stopped {
            ScratchEnd::Clean
        } else {
            ScratchEnd::Abnormal
        };
        let worker_sha = native.metadata()["identity"]["worker_sha256"]
            .as_str()
            .unwrap_or("")
            .to_string();
        drop(native);
        // Only a clean exit has written complete caches (SaveDriverPipelineDiskCache runs at
        // renderer shutdown); a crashed worker never publishes.
        if self.config.shader_seed && end == ScratchEnd::Clean && !worker_sha.is_empty() {
            let seeds = self.config.storage_root.join("shader-seed");
            let root = handle.root.clone();
            let _guard = self.shader_publish.lock().await;
            let published = tokio::task::spawn_blocking(move || {
                crate::shader_seed::publish(&seeds, &worker_sha, &root)
            })
            .await;
            if let Ok(Err(error)) | Err(error) = published.map_err(io::Error::other) {
                tracing::warn!("shader seed not published: {error}");
            }
        }
        // Both tasks observe `closed` at their next wake-up and never wait on another session.
        let tasks = std::mem::take(&mut *lock(&handle.tasks));
        for task in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        }
        self.unregister(id);
        if matches!(lock(&handle.info).kind, SessionKind::Secondary { .. }) {
            self.cards.discard_scratch(&handle.root, end)?;
        }
        result
    }

    fn unregister(&self, id: &str) {
        let mut registry = lock(&self.registry);
        registry.sessions.remove(id);
        registry.mounted.retain(|_, owner| owner != id);
        self.settings_revision.fetch_add(1, Ordering::AcqRel);
        drop(registry);
        self.budgets.release(id);
        let mut focused = lock(&self.focused);
        if focused.as_deref() == Some(id) {
            *focused = None;
        }
        drop(focused);
        lock(&self.workspace).remove(id);
    }

    /// Host memory as admission sees it: the largest worker working set as the estimate.
    #[must_use]
    pub fn host_memory(&self) -> crate::policy::Memory {
        let largest = self
            .summaries()
            .iter()
            .filter_map(|summary| summary.telemetry.memory_bytes)
            .max();
        crate::policy::Memory::measure(self.config.policy.memory_reserve_mib, largest)
    }

    /// Device-local GPU memory over running Vulkan sessions (lotus#46).
    #[must_use]
    pub fn gpu_memory(&self) -> crate::policy::GpuMemory {
        let summaries = self.summaries();
        let policy = &self.config.policy;
        crate::policy::GpuMemory::measure(
            policy.gpu_admission,
            policy.gpu_reserve_mib,
            summaries
                .iter()
                .filter(|summary| summary.telemetry.state.is_active())
                .filter_map(|summary| summary.telemetry.gpu_memory.as_ref()),
        )
    }

    /// Workload policy, budget holders, snapshot queue and memory for `lotus_status`.
    #[must_use]
    pub fn policy_status(&self) -> serde_json::Value {
        self.budgets.status(&self.host_memory(), &self.gpu_memory())
    }

    /// The session a person is looking at (desktop selection, `lotus_focus_session`). With
    /// `policy.focus_priority` its worker runs in the above-normal priority class and the
    /// previously focused worker returns to normal. Nothing changes otherwise.
    pub fn set_focus(&self, id: &str) {
        if !self.config.policy.focus_priority {
            return;
        }
        let previous = lock(&self.focused).replace(id.to_string());
        if previous.as_deref() == Some(id) {
            return;
        }
        for (session, raised) in previous
            .iter()
            .map(|p| (p.as_str(), false))
            .chain([(id, true)])
        {
            let Some(pid) = self.summary(session).ok().and_then(|s| s.telemetry.pid) else {
                continue;
            };
            // The supervisor holds the child's process handle, so this PID is not reused.
            if let Err(error) = crate::policy::set_priority(pid, raised) {
                tracing::warn!("worker {pid} priority not changed: {error}");
            }
        }
    }

    /// Failure injection and emergency termination, limited to this session's job.
    pub async fn abort(&self, id: &str, actor: Actor) -> io::Result<()> {
        self.require_control()?;
        let handle = self.handle(id)?;
        handle.set_playing(false);
        handle.record(actor, "abort");
        handle.native.lock().await.abort().await;
        Ok(())
    }

    /// Query the worker at a safe point and refresh gauges.
    pub async fn status(&self, id: &str) -> io::Result<serde_json::Value> {
        let handle = self.handle(id)?;
        let mut native = handle.native.lock().await;
        let status = native.status().await?;
        observe(&handle, &native, &status);
        Ok(status)
    }

    /// Typed debugger entry point shared by CLI, MCP, desktop and GDB.
    pub async fn debug(
        &self,
        id: &str,
        command: &lotus_protocol::debug::DebugCommand,
        actor: Actor,
    ) -> io::Result<serde_json::Value> {
        self.debug_as(id, command, actor_client(actor)).await
    }

    /// Dispatch against the guest that supplied a desktop inspector action.
    /// The revision is checked with the same lock that serializes worker
    /// commands, including reset and load. CLI and MCP keep their existing
    /// session-scoped dispatch through `debug_as`.
    pub async fn debug_at(
        &self,
        id: &str,
        created_unix_ms: u64,
        epoch: u64,
        command: &lotus_protocol::debug::DebugCommand,
        actor: Actor,
    ) -> io::Result<serde_json::Value> {
        self.debug_as_checked(
            id,
            command,
            actor_client(actor),
            DebugScope {
                target: Some((created_unix_ms, epoch)),
                cancellation: None,
                precondition: DebugPrecondition::None,
            },
        )
        .await
    }

    /// Cancel queued desktop work, or drain a dispatched operation before
    /// releasing its worker lock. The caller keeps polling this future until it returns.
    pub async fn debug_at_cancellable(
        &self,
        id: &str,
        expected: (u64, u64),
        command: &lotus_protocol::debug::DebugCommand,
        actor: Actor,
        cancellation: &DebugCancellation,
        precondition: DebugPrecondition<'_>,
    ) -> io::Result<serde_json::Value> {
        let scope = DebugScope {
            target: Some(expected),
            cancellation: Some(cancellation),
            precondition,
        };
        let mut phase = cancellation.0.subscribe();
        tokio::select! {
            biased;
            _ = phase.wait_for(|phase| *phase == DebugPhase::Cancelled) => Err(debug_cancelled()),
            result = self.debug_as_checked(id, command, actor_client(actor), scope) => result,
        }
    }

    pub async fn subscribe(
        &self,
        id: &str,
    ) -> io::Result<tokio::sync::broadcast::Receiver<serde_json::Value>> {
        Ok(self.handle(id)?.native.lock().await.subscribe())
    }

    /// Connection-specific identity is supplied by a trusted adapter, never by a tool argument.
    pub async fn debug_as(
        &self,
        id: &str,
        command: &lotus_protocol::debug::DebugCommand,
        client: &str,
    ) -> io::Result<serde_json::Value> {
        self.debug_as_checked(id, command, client, DebugScope::default())
            .await
    }

    /// A connection-specific actor whose debugger is bound to one guest epoch.
    pub async fn debug_as_at(
        &self,
        id: &str,
        expected: (u64, u64),
        command: &lotus_protocol::debug::DebugCommand,
        client: &str,
    ) -> io::Result<serde_json::Value> {
        self.debug_as_checked(
            id,
            command,
            client,
            DebugScope {
                target: Some(expected),
                cancellation: None,
                precondition: DebugPrecondition::None,
            },
        )
        .await
    }

    /// Release only this connection's lease on the original session. This must
    /// work during teardown even when the original guest epoch has expired.
    pub async fn release_debug_lease_at(
        &self,
        id: &str,
        created_unix_ms: u64,
        client: &str,
    ) -> io::Result<()> {
        let handle = self.handle(id)?;
        let _native = handle.native.lock().await;
        if handle.info().created_unix_ms != created_unix_ms {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "debug session changed",
            ));
        }
        release_debug_lease(&handle, client, client)
    }

    async fn debug_as_checked(
        &self,
        id: &str,
        command: &lotus_protocol::debug::DebugCommand,
        client: &str,
        scope: DebugScope<'_>,
    ) -> io::Result<serde_json::Value> {
        use crate::policy::Tool;
        use lotus_protocol::debug::{DebugCommand, GpuCapture};
        command.validate().map_err(io::Error::other)?;
        if debug_requires_control(command) {
            self.require_control()?;
        }
        let handle = self.handle(id)?;
        // Host-wide budgets (crate::policy). Session-long tools take a permit before any
        // await and record it after the worker accepted; one-shot full-state work holds a slot.
        let admission = match command {
            DebugCommand::RewindConfigure {
                enabled,
                budget_mib,
                compression_queue,
                ..
            } => Some((
                self.budgets
                    .reserve_rewind(id, *enabled, *budget_mib, *compression_queue),
                *enabled,
            )),
            DebugCommand::GpuConfigure {
                enabled, capture, ..
            } => {
                let active = *enabled && *capture != Some(GpuCapture::Counters);
                Some((self.budgets.reserve(Tool::GpuCapture, id, active), active))
            }
            _ => None,
        };
        let permit = match admission {
            Some((reservation, enable)) => match reservation {
                Ok(permit) => Some((permit, enable)),
                Err(error) => {
                    handle.telemetry().policy_note = error.to_string();
                    return Err(error);
                }
            },
            None => None,
        };
        let slot = match command {
            DebugCommand::SaveState { .. }
            | DebugCommand::LoadState { .. }
            | DebugCommand::CitraceStart
            | DebugCommand::CitraceReplay
            | DebugCommand::Fork { .. }
            | DebugCommand::Matrix { .. } => {
                let slot = self.budgets.snapshot(command_name(command)).await;
                handle.telemetry().policy_note = match &slot {
                    Ok(slot) if !slot.waited.is_zero() => format!(
                        "{} waited {} ms for a snapshot slot",
                        command_name(command),
                        slot.waited.as_millis()
                    ),
                    Ok(_) => String::new(),
                    Err(error) => error.to_string(),
                };
                Some(slot?)
            }
            _ => None,
        };
        let mut result = self.debug_inner(id, command, client, scope, permit).await?;
        drop(slot);
        handle.native.lock().await.decorate(&mut result);
        Ok(result)
    }

    async fn debug_inner(
        &self,
        id: &str,
        command: &lotus_protocol::debug::DebugCommand,
        client: &str,
        scope: DebugScope<'_>,
        permit: Option<(crate::policy::ToolPermit<'_>, bool)>,
    ) -> io::Result<serde_json::Value> {
        use lotus_protocol::debug::DebugCommand;
        let handle = self.handle(id)?;
        // Host presentation and orchestration operations own their own
        // lock lifetimes. Check their scoped target before routing them.
        if scope.target.is_some()
            && matches!(
                command,
                DebugCommand::AudioFocus { .. }
                    | DebugCommand::CaptureScaled { .. }
                    | DebugCommand::Matrix { .. }
            )
        {
            let native = handle.native.lock().await;
            check_debug_target(scope.target, handle.info().created_unix_ms, native.epoch())?;
            scope.dispatch()?;
        }
        if let DebugCommand::AudioFocus { enabled } = command {
            return self.focus_audio(id, *enabled).await;
        }
        if let DebugCommand::CaptureScaled { scale } = command {
            let artifacts = self.capture_scaled(id, *scale).await?;
            return Ok(serde_json::json!({"artifacts":artifacts,"scale":scale}));
        }
        if let DebugCommand::Matrix {
            profiles,
            frames,
            checkpoints,
            repeats,
        } = command
        {
            return self
                .matrix(id, profiles, *frames, checkpoints, *repeats, client)
                .await;
        }
        if matches!(command, DebugCommand::Stop) {
            let metadata = {
                let native = handle.native.lock().await;
                check_debug_target(scope.target, handle.info().created_unix_ms, native.epoch())?;
                check_lease(&handle, client)?;
                scope.dispatch()?;
                if !handle.close() {
                    return Err(io::Error::other("session is already stopping"));
                }
                native.metadata()
            };
            self.stop_claimed(id, Actor::Host, handle).await?;
            return Ok(serde_json::json!({"state":"stopped","metadata":metadata}));
        }
        let mut native = handle.native.lock().await;
        check_debug_target(scope.target, handle.info().created_unix_ms, native.epoch())?;
        if scope.target.is_some() && (handle.is_closed() || !native.state().is_active()) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "debug session is stopping",
            ));
        }
        let needs_pause = matches!(
            scope.precondition,
            DebugPrecondition::Paused | DebugPrecondition::MemoryBytes(_)
        ) || matches!(scope.precondition, DebugPrecondition::Thread { .. })
            && !matches!(command, DebugCommand::ThreadRegisters { .. });
        if needs_pause
            && (handle.playing.load(Ordering::Acquire) || native.state() != SessionState::Paused)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Pause the guest before using this inspector action",
            ));
        }
        if let Some(feature) = command.required_feature() {
            native.require(feature)?;
        }
        if let DebugPrecondition::ScanGeneration(generation) = scope.precondition {
            native.check_scan_generation(generation)?;
        }
        scope.dispatch()?;
        if let DebugCommand::StorageBrowse {
            area,
            path,
            offset,
            limit,
        } = command
        {
            let mut result = self.browse_storage(&handle.root, *area, path, *offset, *limit)?;
            native.decorate(&mut result);
            return Ok(result);
        }
        if let DebugCommand::InstallCia { asset } | DebugCommand::ContentTransform { asset, .. } =
            command
        {
            check_lease(&handle, client)?;
            handle.set_playing(false);
            pause_if_running(&mut native).await?;
            let kind = if matches!(command, DebugCommand::InstallCia { .. }) {
                lotus_protocol::debug::AssetKind::Cia
            } else {
                lotus_protocol::debug::AssetKind::Rom
            };
            crate::assets::mount(&handle.root, &self.resolve_asset(asset)?, kind)?;
            let mut result = native.debug(command).await?;
            self.import_debug_artifacts(id, &handle.root, &mut result)?;
            return Ok(result);
        }
        if let DebugCommand::LeaseAcquire { owner } = command {
            if owner != client {
                return Err(io::Error::other("lease owner must match this connection"));
            }
            let mut lease = lock(&handle.lease);
            if lease.as_ref().is_some_and(|owner| owner != client) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "debug lease held by another client",
                ));
            }
            *lease = Some(client.to_string());
            handle.telemetry().desktop_debug_lease_blocked = client != actor_client(Actor::Desktop);
            return Ok(serde_json::json!({"lease": client}));
        }
        if let DebugCommand::Fork { count, overrides } = command {
            check_lease(&handle, client)?;
            let asset = handle
                .info()
                .asset
                .ok_or_else(|| io::Error::other("fork requires approved loaded content"))?;
            if self.sessions().len() + usize::from(*count) > self.max_sessions() {
                return Err(io::Error::other("fork exceeds host session limit"));
            }
            handle.set_playing(false);
            pause_if_running(&mut native).await?;
            let checkpoint = "lotus-fork";
            native
                .debug(&DebugCommand::SaveState {
                    name: checkpoint.into(),
                })
                .await?;
            // The savestate is taken with no FS operation pending (debug.h refuses otherwise);
            // flush so the copied card matches it.
            native.flush_storage().await?;
            let manifest: serde_json::Value =
                serde_json::from_slice(&fs::read(handle.root.join("native-manifest.json"))?)
                    .map_err(io::Error::other)?;
            let profile: BTreeMap<String, lotus_protocol::debug::SettingValue> =
                serde_json::from_value(manifest["settings"].clone()).map_err(io::Error::other)?;
            let mut created = Vec::new();
            let result=async {
                for _ in 0..*count {
                    let sibling=self.create_seeded(SessionKind::Secondary { parent:None },Some(&asset),Actor::Host,
                        Some((&handle.root,&profile)),None).await?;
                    created.push(sibling.clone());
                    let sibling_handle=self.handle(&sibling.id)?;
                    let mut target=sibling_handle.native.lock().await;
                    target.debug(&DebugCommand::LoadState { name:checkpoint.into() }).await?;
                    for (name,value) in overrides {
                        target.debug(&DebugCommand::SettingsSet { name:name.clone(),value:value.clone() }).await?;
                    }
                }
                Ok::<_,io::Error>(serde_json::json!({"forks":created,"source_session":id,"checkpoint":checkpoint,
                    "profile_hash":native.profile_hash(),"paused":true,"safe_point":true,"rasterizer_cache_flush":true}))
            }.await;
            if result.is_err() {
                for sibling in created {
                    let _ = self.stop(&sibling.id, Actor::Host).await;
                }
            }
            return result;
        }
        if let DebugCommand::AssetMount { asset, kind } = command {
            check_lease(&handle, client)?;
            if !matches!(
                kind,
                lotus_protocol::debug::AssetKind::MicWav
                    | lotus_protocol::debug::AssetKind::CameraBmp
                    | lotus_protocol::debug::AssetKind::Nfc
            ) && native.state() != SessionState::Idle
            {
                return Err(io::Error::other(
                    "system assets must be mounted before loading content",
                ));
            }
            handle.set_playing(false);
            pause_if_running(&mut native).await?;
            crate::assets::mount(&handle.root, &self.resolve_asset(asset)?, *kind)?;
            return native.debug(command).await;
        }
        if let DebugCommand::LeaseRelease { owner } = command {
            release_debug_lease(&handle, owner, client)?;
            return Ok(serde_json::json!({"released": client}));
        }
        if let DebugCommand::Disassemble {
            address,
            length,
            thumb,
        } = command
        {
            let result = native
                .debug(&DebugCommand::ReadMemory {
                    address: *address,
                    length: *length,
                })
                .await?;
            if let Some(cancellation) = scope.cancellation {
                cancellation.check()?;
            }
            let bytes: Vec<u8> =
                serde_json::from_value(result["bytes"].clone()).map_err(io::Error::other)?;
            let profile = native.profile_hash().to_owned();
            let source = lock(&handle.symbols)
                .as_ref()
                .filter(|symbols| symbols.epoch == native.epoch())
                .map(|symbols| {
                    Ok::<_, io::Error>((symbols.index.locate(*address)?, symbols.sha256.clone()))
                })
                .transpose()?;
            drop(native);
            let text = crate::disassembly::decode(&handle.root, *address, &bytes, *thumb).await?;
            let mut result = serde_json::json!({"address":address,"bytes":bytes,"thumb":thumb,"assembly":text,
                "paused":result["paused"],"safe_point":true,"rasterizer_cache_flush":true,
                "profile_hash":profile, "epoch":result["epoch"]});
            if let Some((location, sha256)) = source {
                result["source"] = serde_json::to_value(location).map_err(io::Error::other)?;
                result["elf_sha256"] = sha256.into();
            }
            return Ok(result);
        }
        if let DebugCommand::LoadSymbols { asset } = command {
            check_lease(&handle, client)?;
            let path = self.resolve_asset(asset)?;
            if fs::metadata(&path)?.len() > 128 << 20 {
                return Err(io::Error::other("ELF exceeds budget"));
            }
            let bytes = fs::read(path)?;
            let symbols = lotus_debugger::symbols::Symbols::parse(&bytes)?;
            let count = symbols.count();
            let sha256 = format!("{:x}", Sha256::digest(bytes));
            *lock(&handle.symbols) = Some(SymbolBinding {
                index: symbols,
                epoch: native.epoch(),
                sha256: sha256.clone(),
            });
            return Ok(
                serde_json::json!({"symbols":count, "elf_sha256":sha256, "epoch":native.epoch()}),
            );
        }
        if let DebugCommand::Symbolize { addresses } = command {
            let symbols = lock(&handle.symbols);
            let symbols = symbols
                .as_ref()
                .filter(|symbols| symbols.epoch == native.epoch())
                .ok_or_else(|| {
                    io::Error::other(
                        "load an approved ELF build artifact for this guest epoch first",
                    )
                })?;
            let locations: io::Result<Vec<_>> = addresses
                .iter()
                .map(|address| symbols.index.locate(*address))
                .collect();
            return Ok(
                serde_json::json!({"locations":locations?, "elf_sha256":symbols.sha256, "epoch":native.epoch()}),
            );
        }
        if let DebugCommand::ResolveSymbol { query } = command {
            let symbols = lock(&handle.symbols);
            let symbols = symbols
                .as_ref()
                .filter(|symbols| symbols.epoch == native.epoch())
                .ok_or_else(|| {
                    io::Error::other(
                        "load an approved ELF build artifact for this guest epoch first",
                    )
                })?;
            return Ok(
                serde_json::json!({"addresses":symbols.index.resolve(query)?, "elf_sha256":symbols.sha256, "epoch":native.epoch()}),
            );
        }
        // The native mutex serializes lease transitions with execution mutations.
        let memory_check = match (scope.precondition, command) {
            (
                DebugPrecondition::MemoryBytes(original),
                DebugCommand::WriteMemory { address, bytes },
            ) if original.len() == bytes.len() => Some((
                original,
                bytes,
                DebugCommand::ReadMemory {
                    address: *address,
                    length: bytes.len() as u32,
                },
            )),
            (DebugPrecondition::MemoryBytes(_), _) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid memory edit precondition",
                ))
            }
            _ => None,
        };
        if let DebugPrecondition::Thread { process, thread } = scope.precondition {
            if command.mutates() {
                check_lease(&handle, client)?;
            }
            let matches_thread = match command {
                DebugCommand::ThreadRegisters { thread: id }
                | DebugCommand::WriteThreadRegister { thread: id, .. }
                | DebugCommand::Suspend {
                    thread: Some(id), ..
                } => *id == thread,
                DebugCommand::Backtrace { .. } => true,
                _ => false,
            };
            if !matches_thread {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Invalid thread action",
                ));
            }
            let snapshot = native.debug(&DebugCommand::Threads).await?;
            let current = snapshot["threads"].as_array().is_some_and(|threads| {
                threads
                    .iter()
                    .any(|entry| entry["id"] == thread && entry["process"] == process)
            });
            if !current {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "The selected guest thread no longer exists",
                ));
            }
            if let DebugCommand::Backtrace { core, .. } = command {
                let context = native
                    .debug(&DebugCommand::ThreadRegisters { thread })
                    .await?;
                if context["core"] != *core || context["thread"] != thread {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "The selected thread is no longer active on this core",
                    ));
                }
            }
        }
        if command.mutates() {
            check_lease(&handle, client)?;
            if let Some((original, _, read)) = &memory_check {
                let sampled = native.debug(read).await?;
                if !memory_reply_matches(&sampled, original) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Guest bytes changed. Read them again before editing.",
                    ));
                }
            }
            handle.set_playing(false);
            pause_if_running(&mut native).await?;
            handle.record(Actor::Host, client);
            // Load and reset start new content; state loads, replays and input commands set
            // worker input themselves, so later live states still apply but are never deduped.
            if matches!(command, DebugCommand::Load { .. } | DebugCommand::Reset) {
                handle.cancel_input(true);
            } else {
                lock(&handle.mailbox).forget();
            }
        }
        match command {
            DebugCommand::Status => native.status().await,
            DebugCommand::Reset => {
                let result = native.reset().await?;
                handle.telemetry().epoch = native.epoch();
                self.budgets.clear_recordings(id);
                self.settings_revision.fetch_add(1, Ordering::AcqRel);
                Ok(result)
            }
            DebugCommand::Load { asset } => {
                let result = native.load(&self.resolve_asset(asset)?).await?;
                handle.telemetry().epoch = native.epoch();
                self.budgets.clear_recordings(id);
                lock(&handle.info).asset = Some(asset.clone());
                self.settings_revision.fetch_add(1, Ordering::AcqRel);
                Ok(result)
            }
            DebugCommand::Continue => {
                let result = native.run().await?;
                handle.set_playing(true);
                Ok(result)
            }
            DebugCommand::Pause => native.pause().await,
            DebugCommand::Advance { frames } => native.advance(*frames).await,
            _ => {
                let mut result = native.debug(command).await?;
                if let Some((_, bytes, read)) = &memory_check {
                    let readback = native.debug(read).await.map_err(|error| {
                        io::Error::new(
                            error.kind(),
                            format!("Write acknowledged, but readback failed: {error}"),
                        )
                    })?;
                    if !memory_reply_matches(&readback, bytes) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Write acknowledged, but readback differs from the requested bytes",
                        ));
                    }
                    result["readback_verified"] = true.into();
                }
                {
                    let mut telemetry = handle.telemetry();
                    telemetry.epoch = native.epoch();
                    if matches!(command, DebugCommand::GpuConfigure { .. }) {
                        telemetry.gpu_capture =
                            crate::telemetry::AcceptedGpuCapture::deserialize(&result)
                                .ok()
                                .filter(|accepted| {
                                    accepted.epoch == native.epoch()
                                        && accepted.stop_events.len() <= 8
                                })
                                .map(Arc::new);
                    }
                }
                // Keep the ledger in worker command order even when another
                // request acquires this session's native lock immediately.
                if let Some((permit, active)) = permit {
                    permit.commit(active);
                }
                if matches!(
                    command,
                    DebugCommand::SettingsSet { .. } | DebugCommand::SystemRegenerate { .. }
                ) {
                    self.settings_revision.fetch_add(1, Ordering::AcqRel);
                }
                self.import_debug_artifacts(id, &handle.root, &mut result)?;
                if let Some(symbols) = lock(&handle.symbols)
                    .as_ref()
                    .filter(|symbols| symbols.epoch == native.epoch())
                {
                    if let Some(frames) = result
                        .get_mut("frames")
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        for frame in frames {
                            if let Some(pc) = frame["pc"].as_u64() {
                                frame["source"] =
                                    serde_json::to_value(symbols.index.locate(pc as u32)?)
                                        .map_err(io::Error::other)?;
                            }
                        }
                        result["elf_sha256"] = symbols.sha256.clone().into();
                    }
                }
                Ok(result)
            }
        }
    }

    /// Publish files a debug command wrote below the session root (`gpu/`, `audio/`,
    /// `movies/`, `content/`) as host artifacts, replacing `result.artifacts`.
    fn import_debug_artifacts(
        &self,
        id: &str,
        root: &Path,
        result: &mut serde_json::Value,
    ) -> io::Result<()> {
        let Some(files) = result["artifacts"].as_array() else {
            return Ok(());
        };
        if files.len() > crate::artifacts::MAX_BATCH {
            return Err(io::Error::other("artifact batch exceeds budget"));
        }
        let root = root.canonicalize()?;
        let mut batch = Vec::new();
        for file in files {
            let relative = Path::new(
                file["path"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("invalid artifact path"))?,
            );
            if relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
                || !relative.components().next().is_some_and(|part| {
                    ["gpu", "audio", "movies", "content"]
                        .iter()
                        .any(|allowed| part.as_os_str() == std::ffi::OsStr::new(allowed))
                })
            {
                return Err(io::Error::other("artifact path outside capture folders"));
            }
            let path = root.join(relative).canonicalize()?;
            if !path.starts_with(&root) {
                return Err(io::Error::other("artifact escaped session root"));
            }
            if fs::metadata(&path)?.len() > crate::artifacts::MAX_ARTIFACT_BYTES {
                return Err(io::Error::other("artifact byte quota reached"));
            }
            batch.push(NewArtifact {
                bytes: fs::read(&path)?,
                screen: file["format"].as_str().unwrap_or("capture").to_string(),
                width: file["width"].as_u64().unwrap_or(0) as u32,
                height: file["height"].as_u64().unwrap_or(0) as u32,
                mime_type: match file["format"].as_str() {
                    Some("json") => "application/json",
                    Some("wav") => "audio/wav",
                    Some("citrace") => "application/x-citrace",
                    _ => "application/octet-stream",
                }
                .into(),
                extension: "",
            });
        }
        let published = self.publish(id, batch)?;
        result["artifacts"] = serde_json::to_value(published).map_err(io::Error::other)?;
        Ok(())
    }

    /// Reserve room under the artifact quota, write unlocked, then catalogue atomically.
    pub(crate) fn publish(
        &self,
        session: &str,
        batch: Vec<NewArtifact>,
    ) -> io::Result<Vec<Artifact>> {
        if batch
            .iter()
            .any(|item| item.bytes.len() as u64 > crate::artifacts::MAX_ARTIFACT_BYTES)
        {
            return Err(io::Error::other("artifact byte quota reached"));
        }
        let live = self.sessions();
        let total = batch.iter().map(|item| item.bytes.len() as u64).sum();
        let (reservation, directory, retry) = {
            let mut store = lock(&self.artifacts);
            let reservation = store.reserve(session, batch.len(), total, &live, Instant::now())?;
            let retry = store.take_undeleted();
            (reservation, store.directory().to_path_buf(), retry)
        };
        // Evictions are committed; a file that cannot be deleted now is retried next publish.
        let undeleted: Vec<PathBuf> = retry
            .into_iter()
            .chain(reservation.evicted.iter().cloned())
            .filter(|path| {
                fs::remove_file(path).is_err_and(|error| error.kind() != io::ErrorKind::NotFound)
            })
            .collect();
        if !undeleted.is_empty() {
            lock(&self.artifacts).defer_delete(undeleted);
        }
        match crate::artifacts::write_batch(&directory, session, batch) {
            Ok(written) => Ok(lock(&self.artifacts).commit(&reservation, written, Instant::now())),
            Err(error) => {
                lock(&self.artifacts).abandon(&reservation);
                Err(error)
            }
        }
    }

    /// Host presentation policy; never pauses the guest or changes deterministic capture.
    pub async fn focus_audio(&self, id: &str, enabled: bool) -> io::Result<serde_json::Value> {
        use lotus_protocol::debug::DebugCommand;
        self.require_control()?;
        let handle = self.handle(id)?;
        // The latest request owns focus at once (#45). Exchanges on one worker are FIFO on
        // its session lock, so a later hand-off away from `id` is applied after this one.
        // The previous holder is muted concurrently: a stalled worker delays only its own
        // mute, and may keep audio until it answers.
        let previous = {
            let mut focus = lock(&self.audio_focus);
            if enabled {
                focus.replace(id.to_string()).filter(|old| old != id)
            } else {
                if focus.as_deref() == Some(id) {
                    *focus = None;
                }
                None
            }
        };
        let mute = async {
            let Some(old) = previous.and_then(|previous| self.handle(&previous).ok()) else {
                return Ok(serde_json::Value::Null);
            };
            let mut native = old.native.lock().await;
            native
                .debug(&DebugCommand::AudioFocus { enabled: false })
                .await
        };
        let apply = async {
            handle
                .native
                .lock()
                .await
                .debug(&DebugCommand::AudioFocus { enabled })
                .await
        };
        let (muted, result) = tokio::join!(mute, apply);
        if result.is_err() && enabled {
            let mut focus = lock(&self.audio_focus);
            if focus.as_deref() == Some(id) {
                *focus = None;
            }
        }
        let result = result?;
        muted?;
        Ok(result)
    }

    pub async fn registers(&self, id: &str, core: u32) -> io::Result<serde_json::Value> {
        let handle = self.handle(id)?;
        let mut native = handle.native.lock().await;
        native.registers(core).await
    }

    pub async fn memory(
        &self,
        id: &str,
        address: u32,
        length: u16,
    ) -> io::Result<serde_json::Value> {
        let handle = self.handle(id)?;
        let mut native = handle.native.lock().await;
        native.memory(address, length).await
    }

    /// Make the next [`Self::preview`] capture even if the paused session is unchanged
    /// (a frontend that dropped its images, e.g. after switching sessions).
    pub fn forget_preview(&self, id: &str) {
        if let Ok(handle) = self.handle(id) {
            *lock(&handle.previewed) = None;
        }
    }

    /// Latest screens for display, or `None` when the session is busy, blank, or paused
    /// with nothing new since the last preview.
    pub async fn preview(&self, id: &str) -> Option<Vec<Frame>> {
        let handle = self.handle(id).ok()?;
        let mut native = handle.native.try_lock().ok()?;
        let (frames, epoch) = {
            let telemetry = handle.telemetry();
            (telemetry.frames, native.epoch())
        };
        if !matches!(native.state(), SessionState::Running | SessionState::Paused) || frames == 0 {
            return None;
        }
        // Between bounded runs, keep polling for completed readbacks without a GPU wait.
        // A manual stop needs one exact frame until its frame or epoch changes.
        let key = (epoch, frames);
        let playing = handle.is_playing();
        let paused = !playing && native.state() == SessionState::Paused;
        if paused && *lock(&handle.previewed) == Some(key) {
            return None;
        }
        let preview = native.preview_with_policy(playing).await;
        handle.telemetry().preview = native.preview_metrics().clone();
        let (captured, _) = preview.ok()?;
        *lock(&handle.previewed) = paused.then_some(key);
        // A repeated slot or no frame yet: the window keeps what it shows (lotus#25).
        native.preview_metrics().fresh.then_some(captured)
    }

    pub async fn capture(&self, id: &str) -> io::Result<Vec<Artifact>> {
        self.capture_scaled(id, 1).await
    }
    pub async fn capture_scaled(&self, id: &str, scale: u32) -> io::Result<Vec<Artifact>> {
        let frames = self.capture_frames(id, scale).await?;
        let batch = frames
            .into_iter()
            .map(|frame| NewArtifact {
                bytes: frame.bgra,
                screen: frame.lease.screen,
                width: frame.lease.width,
                height: frame.lease.height,
                mime_type: "application/octet-stream".into(),
                extension: "bgra",
            })
            .collect();
        self.publish(id, batch)
    }

    /// Capture owned, exact pixels using the same size and lease checks as artifact capture.
    pub async fn capture_frames(&self, id: &str, scale: u32) -> io::Result<Vec<Frame>> {
        let handle = self.handle(id)?;
        let frames = handle.native.lock().await.capture_scaled(scale).await?;
        Ok(frames)
    }

    #[must_use]
    pub fn artifacts(&self) -> Vec<Artifact> {
        lock(&self.artifacts).list()
    }

    /// Resolve only catalogued immutable captures, never interpret a URI as a path. An
    /// evicted or released URI fails with `NotFound` and says so.
    pub fn read_artifact(&self, uri: &str) -> io::Result<Vec<u8>> {
        let (artifact, path) = lock(&self.artifacts).open_artifact(uri)?;
        let data = fs::read(path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                io::Error::new(io::ErrorKind::NotFound, "artifact expired while being read")
            } else {
                error
            }
        })?;
        if data.len() != artifact.bytes || format!("{:x}", Sha256::digest(&data)) != artifact.sha256
        {
            return Err(io::Error::other("artifact identity mismatch"));
        }
        Ok(data)
    }

    /// Read bounded desktop captures with cancellation points between disk reads.
    pub fn read_debug_artifact(
        &self,
        session: &str,
        uri: &str,
        limit: usize,
        cancellation: &DebugCancellation,
    ) -> io::Result<Vec<u8>> {
        cancellation.check()?;
        let (artifact, path) = lock(&self.artifacts).open_artifact(uri)?;
        if artifact.session != session {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture artifact belongs to another session",
            ));
        }
        if artifact.bytes > limit {
            return Err(io::Error::other("desktop artifact exceeds size limit"));
        }
        let mut file = fs::File::open(path)?;
        let mut data = Vec::with_capacity(artifact.bytes);
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            cancellation.check()?;
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            if read > artifact.bytes - data.len() {
                return Err(io::Error::other("artifact identity mismatch"));
            }
            hash.update(&buffer[..read]);
            data.extend_from_slice(&buffer[..read]);
        }
        if data.len() != artifact.bytes || format!("{:x}", hash.finalize()) != artifact.sha256 {
            return Err(io::Error::other("artifact identity mismatch"));
        }
        Ok(data)
    }

    /// Delete artifacts now. All URIs must be catalogued, or nothing is released.
    pub fn release_artifacts(&self, uris: &[String]) -> io::Result<usize> {
        self.require_control()?;
        let files = lock(&self.artifacts).release(uris)?;
        for path in &files {
            let _ = fs::remove_file(path);
        }
        Ok(files.len())
    }

    /// Keep an artifact until released (or unpin it). Limited to the session's floor.
    pub fn pin_artifact(&self, uri: &str, pinned: bool) -> io::Result<Artifact> {
        self.require_control()?;
        lock(&self.artifacts).pin(uri, pinned)
    }

    /// Catalogue usage against the quota, per session.
    #[must_use]
    pub fn artifact_usage(&self) -> crate::artifacts::Usage {
        let live = self.sessions();
        lock(&self.artifacts).usage(&live)
    }

    /// Ask a windowed frontend to show `id`. Headless hosts ignore it.
    pub fn request_focus(&self, id: &str) -> io::Result<()> {
        self.handle(id)?;
        self.set_focus(id);
        *lock(&self.focus_request) = Some(id.to_string());
        Ok(())
    }

    /// Called once by a windowed frontend; workspace changes are refused until then.
    pub fn enable_workspace(&self, defaults: crate::workspace::SplitDefaults) {
        lock(&self.workspace).set_defaults(defaults);
        self.workspace_available.store(true, Ordering::Release);
    }

    /// Window layout for `lotus_status` and `lotus_workspace get`.
    #[must_use]
    pub fn workspace(&self) -> crate::workspace::Status {
        let workspace = lock(&self.workspace);
        let snapshot = workspace.snapshot();
        let resolved = lock(&self.workspace_resolved)
            .filter(|resolved| resolved.revision == snapshot.revision);
        crate::workspace::Status {
            available: self.workspace_available.load(Ordering::Acquire),
            resolved: resolved
                .filter(|_| matches!(snapshot.layout, crate::workspace::Layout::Split { .. })),
            snapshot,
            defaults: workspace.defaults(),
        }
    }

    /// The window reports how it resolved `Auto` for the current revision (lotus#69).
    /// Display only: never changes the workspace revision, a worker or renderer settings.
    pub fn report_workspace_resolved(&self, resolved: crate::workspace::Resolved) {
        *lock(&self.workspace_resolved) = Some(resolved);
    }

    /// Commits one workspace operation atomically or refuses it. MCP needs control
    /// permission. Never touches a worker, the inspector target or audio focus.
    pub fn update_workspace(
        &self,
        operation: &crate::workspace::Operation,
        expected_revision: Option<u64>,
        actor: Actor,
    ) -> io::Result<crate::workspace::Snapshot> {
        if !self.workspace_available.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no LOTUS window: workspace layout exists only in lotus-desktop",
            ));
        }
        if actor == Actor::Mcp {
            self.require_control()?;
        }
        let registry = lock(&self.registry);
        let result = lock(&self.workspace).apply(operation, expected_revision, |id| {
            registry.sessions.contains_key(id)
        });
        drop(registry);
        result.map_err(|error| {
            let kind = match error {
                crate::workspace::WorkspaceError::Stale { .. } => io::ErrorKind::ResourceBusy,
                crate::workspace::WorkspaceError::UnknownSession(_) => io::ErrorKind::NotFound,
                _ => io::ErrorKind::InvalidInput,
            };
            io::Error::new(kind, error.to_string())
        })
    }

    #[must_use]
    pub fn take_focus_request(&self) -> Option<String> {
        lock(&self.focus_request).take()
    }

    /// Stop every session concurrently and refuse new ones; used when a frontend closes.
    ///
    /// Each stop has its own deadline and forced termination ([`Self::stop`]), so the whole
    /// close is bounded by one worker's deadline, not by the session count. Returns each
    /// session's outcome; a stop still unfinished at the host deadline is abandoned and
    /// reported (its worker was already terminated).
    pub async fn close(self: &Arc<Self>) -> Vec<(String, io::Result<()>)> {
        self.closing.store(true, Ordering::Release);
        let mut stops = tokio::task::JoinSet::new();
        let mut waiting: std::collections::BTreeSet<String> = self.sessions().into_iter().collect();
        for id in &waiting {
            let host = self.clone();
            let id = id.clone();
            stops.spawn(async move {
                let result = host.stop_session(&id, Actor::Host).await;
                (id, result)
            });
        }
        let deadline = tokio::time::Instant::now()
            + CLOSE_DEADLINE
            + RELEASE_DEADLINE
            + Duration::from_secs(1);
        let mut outcomes = Vec::new();
        loop {
            match tokio::time::timeout_at(deadline, stops.join_next()).await {
                Ok(Some(Ok((id, result)))) => {
                    waiting.remove(&id);
                    outcomes.push((id, result));
                }
                Ok(Some(Err(error))) => {
                    outcomes.push((String::new(), Err(io::Error::other(error))));
                }
                Ok(None) => break,
                Err(_) => {
                    stops.abort_all();
                    break;
                }
            }
        }
        for id in waiting {
            outcomes.push((
                id,
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "host shutdown deadline reached before this stop finished",
                )),
            ));
        }
        outcomes
    }

    /// Replace the frontend's self-reported loop metrics (desktop only).
    pub fn set_frontend_metrics(&self, metrics: serde_json::Value) {
        *lock(&self.frontend_metrics) = metrics;
    }

    #[must_use]
    pub fn frontend_metrics(&self) -> serde_json::Value {
        lock(&self.frontend_metrics).clone()
    }

    /// Whether [`Self::close`] has started.
    #[must_use]
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    fn handles(&self) -> Vec<Arc<SessionHandle>> {
        lock(&self.registry).sessions.values().cloned().collect()
    }

    fn handle(&self, id: &str) -> io::Result<Arc<SessionHandle>> {
        lock(&self.registry)
            .sessions
            .get(id)
            .cloned()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unknown session for this host; session handles expire when the server exits. Create a new session; use a primary card to keep guest storage",
                )
            })
    }

    fn reserve(&self, id: &str, kind: &SessionKind) -> io::Result<Reservation<'_>> {
        let mut registry = lock(&self.registry);
        if self.is_closing() {
            return Err(io::Error::other("host is shutting down"));
        }
        if registry.sessions.len() + registry.pending >= self.config.max_sessions {
            return Err(io::Error::new(
                io::ErrorKind::QuotaExceeded,
                format!("{}-session limit reached", self.config.max_sessions),
            ));
        }
        let card = match kind {
            SessionKind::Primary { card } => {
                crate::card_store::validate_name(card)?;
                if registry.mounted.contains_key(card) {
                    return Err(io::Error::new(
                        io::ErrorKind::ResourceBusy,
                        format!("card '{card}' already has a primary session"),
                    ));
                }
                if registry.cloning.contains_key(card) {
                    return Err(io::Error::new(
                        io::ErrorKind::ResourceBusy,
                        format!("card '{card}' is being copied for a secondary; retry"),
                    ));
                }
                registry.mounted.insert(card.clone(), id.to_string());
                Some(card.clone())
            }
            SessionKind::Secondary { parent } => {
                if let Some(parent) = parent {
                    crate::card_store::validate_name(parent)?;
                }
                None
            }
        };
        registry.pending += 1;
        Ok(Reservation {
            supervisor: self,
            id: id.to_string(),
            card,
            committed: false,
        })
    }

    /// Copy `card` into a new scratch root at one guest timeline point (#28).
    ///
    /// With a live primary on this host, its session is held for the whole copy: a running
    /// guest is paused, then `flush_storage` writes stdio buffers out. While a guest FS
    /// operation is still pending on a host thread, a guest that was running advances one
    /// frame and retries (up to [`QUIESCE_FRAMES`]); a guest the user paused is never advanced
    /// and the copy is refused instead. The primary resumes afterwards if it was playing.
    /// Without a primary the card is copied while primaries are refused; a card held by
    /// another LOTUS process is refused, since its writes cannot be coordinated.
    async fn clone_card(&self, id: &str, card: &str) -> io::Result<PathBuf> {
        crate::card_store::validate_name(card)?;
        // A primary that reserved the card but has not committed its creation has no session
        // yet: wait for it to commit (then copy through it) or fail (then copy the card),
        // bounded by the worker's 30 s spawn exchange deadline.
        let waiting = Instant::now();
        let primary = loop {
            {
                let mut registry = lock(&self.registry);
                match registry.mounted.get(card).cloned() {
                    None => {
                        *registry.cloning.entry(card.to_string()).or_default() += 1;
                        break None;
                    }
                    Some(primary) if registry.sessions.contains_key(&primary) => {
                        break Some(primary)
                    }
                    Some(_) => {}
                }
            }
            if waiting.elapsed() > MOUNT_WAIT {
                return Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    format!("card '{card}' is still being mounted by its primary; retry"),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let Some(primary) = primary else {
            let result = if self.cards.card_in_use(card)? {
                Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    format!("card '{card}' is mounted by another LOTUS process; its guest writes cannot be paused from here"),
                ))
            } else {
                let cards = self.cards.clone();
                let (id, card) = (id.to_string(), card.to_string());
                tokio::task::spawn_blocking(move || cards.create_scratch(&id, Some(&card)))
                    .await
                    .map_err(io::Error::other)?
            };
            let mut registry = lock(&self.registry);
            if let Some(count) = registry.cloning.get_mut(card) {
                *count -= 1;
                if *count == 0 {
                    registry.cloning.remove(card);
                }
            }
            return result;
        };
        let handle = self.handle(&primary).map_err(|_| {
            io::Error::new(
                io::ErrorKind::ResourceBusy,
                format!("card '{card}' is still being mounted by its primary; retry"),
            )
        })?;
        let mut native = handle.native.lock().await;
        let resume = handle.is_playing();
        let result = async {
            pause_if_running(&mut native).await?;
            let mut frames = 0;
            loop {
                match native.flush_storage().await {
                    Ok(()) => break,
                    Err(error)
                        if error.kind() == io::ErrorKind::ResourceBusy
                            && resume
                            && frames < QUIESCE_FRAMES =>
                    {
                        native.advance(1).await?;
                        frames += 1;
                    }
                    Err(error) if error.kind() == io::ErrorKind::ResourceBusy => {
                        return Err(io::Error::new(
                            io::ErrorKind::ResourceBusy,
                            format!("card '{card}': {error}; the primary is paused mid-operation, run it briefly and retry"),
                        ))
                    }
                    Err(error) => return Err(error),
                }
            }
            let cards = self.cards.clone();
            let (id, card) = (id.to_string(), card.to_string());
            tokio::task::spawn_blocking(move || cards.create_scratch(&id, Some(&card)))
                .await
                .map_err(io::Error::other)?
        }
        .await;
        if resume && native.state() == SessionState::Paused {
            if let Err(error) = native.run().await {
                handle.set_playing(false);
                handle.telemetry().last_error = error.to_string();
            }
        }
        result
    }

    /// Copy the shader seed of `executable` into a root that has no `shaders/` yet.
    async fn seed_shaders(&self, executable: &Path, root: &Path) -> io::Result<&'static str> {
        let seeds = self.config.storage_root.join("shader-seed");
        let (executable, root) = (executable.to_path_buf(), root.to_path_buf());
        tokio::task::spawn_blocking(move || {
            let sha = crate::native_session::worker_hash(&executable)?;
            match crate::shader_seed::seed(&seeds, &sha, &root) {
                Ok(state) => Ok(state),
                Err(error) => {
                    tracing::warn!(
                        "shader seed unavailable, starting with an empty cache: {error}"
                    );
                    Ok("cold")
                }
            }
        })
        .await
        .map_err(io::Error::other)?
    }

    /// Canonical file behind an approved handle, for identity comparisons and
    /// display names. Not a capability: loading still goes through approval.
    #[must_use]
    pub fn asset_path(&self, handle: &str) -> Option<PathBuf> {
        let path = self.resolve_asset(handle).ok()?;
        Some(path.canonicalize().unwrap_or(path))
    }

    fn resolve_asset(&self, handle: &str) -> io::Result<PathBuf> {
        if let Some(path) = self.config.assets.get(handle) {
            return Ok(path.clone());
        }
        if let Some(path) = lock(&self.local_assets).get(handle) {
            return Ok(path.clone());
        }
        if let Some((directory, name)) = handle.split_once('/') {
            if let Some(root) = self.config.asset_directories.get(directory) {
                if is_content_name(name) && !name.contains(['/', '\\']) && name != ".." {
                    let root = root.canonicalize()?;
                    let path = root.join(name).canonicalize()?;
                    if path.parent() == Some(root.as_path()) && path.is_file() {
                        return Ok(path);
                    }
                }
            }
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unknown asset handle",
        ))
    }
}

/// Path-free failure summary for telemetry (lotus#47).
pub(crate) fn failure_value(report: &crate::failure::FailureReport) -> serde_json::Value {
    serde_json::json!({
        "cause": report.cause,
        "summary": report.summary(),
        "renderer": report.cause.is_renderer(),
        "exit_code": report.exit_code,
        "gpu": report.gpu,
    })
}

/// `path.canonicalize()` with an error naming the host configuration key and path.
fn canonical(key: &str, path: &Path) -> io::Result<PathBuf> {
    path.canonicalize()
        .map_err(|error| labeled(key, path, error))
}

fn labeled(key: &str, path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{key}: {}: {error}", path.display()))
}

/// Holds a session slot and card mount during a slow spawn; released on failure.
struct Reservation<'a> {
    supervisor: &'a Supervisor,
    id: String,
    card: Option<String>,
    committed: bool,
}

impl Reservation<'_> {
    fn commit(mut self, handle: Arc<SessionHandle>) {
        let mut registry = lock(&self.supervisor.registry);
        registry.pending -= 1;
        registry.sessions.insert(self.id.clone(), handle);
        self.committed = true;
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let mut registry = lock(&self.supervisor.registry);
        registry.pending -= 1;
        if let Some(card) = &self.card {
            registry.mounted.remove(card);
        }
    }
}

fn check_debug_target(expected: Option<(u64, u64)>, created: u64, epoch: u64) -> io::Result<()> {
    if expected.is_some_and(|target| target != (created, epoch)) {
        return Err(io::Error::other(
            "debugger action expired because the session or guest changed",
        ));
    }
    Ok(())
}

fn observe(handle: &SessionHandle, native: &NativeSession, status: &serde_json::Value) {
    if !native.state().is_active() {
        handle.release_tool_budgets();
    }
    let mut telemetry = handle.telemetry();
    telemetry.playing = handle.is_playing();
    telemetry.observe(status, native.state(), native.epoch(), Instant::now());
    telemetry.pid = native.pid();
    telemetry.memory_bytes = native.working_set();
}

/// Operation name of a debug command, as in its JSON `operation` tag.
fn command_name(command: &lotus_protocol::debug::DebugCommand) -> &'static str {
    use lotus_protocol::debug::DebugCommand;
    match command {
        DebugCommand::SaveState { .. } => "save_state",
        DebugCommand::LoadState { .. } => "load_state",
        DebugCommand::Fork { .. } => "fork",
        DebugCommand::Matrix { .. } => "matrix",
        _ => "debug command",
    }
}

fn memory_reply_matches(value: &serde_json::Value, expected: &[u8]) -> bool {
    value["bytes"].as_array().is_some_and(|bytes| {
        bytes.len() == expected.len()
            && bytes
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.as_u64() == Some(u64::from(*expected)))
    })
}

async fn pause_if_running(native: &mut NativeSession) -> io::Result<()> {
    if native.state() == SessionState::Running {
        native.pause().await?;
    }
    Ok(())
}

fn is_content_name(name: &str) -> bool {
    name.len() > ".3dsx".len()
        && name
            .get(name.len() - 5..)
            .is_some_and(|extension| extension.eq_ignore_ascii_case(".3dsx"))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Registry state stays consistent across a panicking holder: every critical
/// section is a single map update, so recover the guard instead of cascading.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn actor_client(actor: Actor) -> &'static str {
    match actor {
        Actor::Host => "host",
        Actor::Desktop => "desktop",
        Actor::Mcp => "mcp",
    }
}

fn release_debug_lease(handle: &SessionHandle, owner: &str, client: &str) -> io::Result<()> {
    let mut lease = lock(&handle.lease);
    if owner != client || lease.as_deref() != Some(client) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "debug lease is not owned by this client",
        ));
    }
    *lease = None;
    handle.telemetry().desktop_debug_lease_blocked = false;
    Ok(())
}

fn check_lease(handle: &SessionHandle, client: &str) -> io::Result<()> {
    if lock(&handle.lease)
        .as_ref()
        .is_some_and(|owner| owner != client)
    {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "debug lease held by another client",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, control: bool) -> (PathBuf, Supervisor) {
        let base =
            std::env::temp_dir().join(format!("lotus-host-{name}-{}", SessionUuid::generate()));
        fs::create_dir_all(base.join("content")).expect("content dir");
        fs::write(base.join("content/demo.3dsx"), b"3DSX").expect("content");
        fs::write(base.join("content/notes.txt"), b"x").expect("other");
        let mut config = HostConfig::new(std::env::current_exe().expect("exe"), base.join("store"));
        config
            .asset_directories
            .insert("builds".into(), base.join("content"));
        config.allow_control = control;
        (base, Supervisor::new(config).expect("host"))
    }

    #[test]
    fn asset_directories_list_only_direct_3dsx_handles() {
        let (base, host) = host("assets", true);
        assert_eq!(host.assets(), vec!["builds/demo.3dsx".to_string()]);
        assert!(host.resolve_asset("builds/demo.3dsx").is_ok());
        for bad in [
            "builds/notes.txt",
            "builds/../content/demo.3dsx",
            "builds/sub/demo.3dsx",
            "other/demo.3dsx",
            "C:/Windows/notepad.exe",
        ] {
            assert!(host.resolve_asset(bad).is_err(), "resolved {bad}");
        }
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn missing_asset_directory_is_listed_once_created() {
        let base =
            std::env::temp_dir().join(format!("lotus-host-late-{}", SessionUuid::generate()));
        let late = base.join("late");
        let mut config = HostConfig::new(std::env::current_exe().expect("exe"), base.join("store"));
        config.asset_directories.insert("late".into(), late.clone());
        let host = Supervisor::new(config).expect("missing directory is not fatal");
        assert!(host.assets().is_empty());
        let unavailable = host.unavailable_asset_directories();
        assert!(unavailable["late"].contains("late"), "{unavailable:?}");
        assert!(host.resolve_asset("late/demo.3dsx").is_err());
        fs::create_dir_all(&late).expect("late dir");
        fs::write(late.join("demo.3dsx"), b"3DSX").expect("content");
        assert_eq!(host.assets(), vec!["late/demo.3dsx".to_string()]);
        assert!(host.resolve_asset("late/demo.3dsx").is_ok());
        assert!(host.unavailable_asset_directories().is_empty());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn missing_worker_error_names_the_key_and_path() {
        let config = HostConfig::new("no-such-worker.exe", std::env::temp_dir());
        let error = Supervisor::new(config).err().expect("missing worker fails");
        let text = error.to_string();
        assert!(
            text.starts_with("worker: ") && text.contains("no-such-worker.exe"),
            "{text}"
        );
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn local_approval_is_stable_and_rejects_other_files() {
        let (base, host) = host("local", true);
        let path = base.join("content/demo.3dsx");
        let first = host.approve_local_asset(&path).expect("approve");
        assert_eq!(first, "local/demo.3dsx");
        assert_eq!(host.approve_local_asset(&path).expect("again"), first);
        assert!(host
            .approve_local_asset(&base.join("content/notes.txt"))
            .is_err());
        assert!(host.resolve_asset(&first).is_ok());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn reservations_enforce_limits_and_single_primary_mount() {
        let (base, host) = host("reserve", true);
        let primary = SessionKind::Primary { card: "dev".into() };
        let held = host.reserve("a", &primary).expect("first mount");
        assert_eq!(
            host.reserve("b", &primary).err().map(|error| error.kind()),
            Some(io::ErrorKind::ResourceBusy)
        );
        drop(held);
        let _again = host.reserve("c", &primary).expect("released on failure");
        let secondary = SessionKind::Secondary { parent: None };
        let _s1 = host.reserve("d", &secondary).expect("slot 2");
        let _s2 = host.reserve("e", &secondary).expect("slot 3");
        let _s3 = host.reserve("f", &secondary).expect("slot 4");
        assert_eq!(
            host.reserve("g", &secondary)
                .err()
                .map(|error| error.kind()),
            Some(io::ErrorKind::QuotaExceeded)
        );
        assert!(host
            .reserve(
                "h",
                &SessionKind::Secondary {
                    parent: Some("../x".into())
                }
            )
            .is_err());
        let _ = fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn control_permission_gates_every_mutation() {
        let (base, host) = host("readonly", false);
        let kind = SessionKind::Secondary { parent: None };
        assert!(host.create(kind, None, Actor::Mcp).await.is_err());
        assert!(host.run("x", Actor::Mcp).await.is_err());
        assert!(host.stop("x", Actor::Mcp).await.is_err());
        assert!(host.status("x").await.is_err());
        assert!(host.read_artifact("lotus://artifact/../x").is_err());
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn concurrent_publishers_stay_within_the_artifact_quota() {
        let (base, first) = host("artifacts", true);
        drop(first);
        let mut config = HostConfig::new(std::env::current_exe().expect("exe"), base.join("store"));
        config.allow_control = true;
        config.artifacts = ArtifactQuota {
            max_count: 16,
            max_bytes: 1 << 20,
            lease_seconds: 0,
        };
        let host = Arc::new(Supervisor::new(config).expect("host"));
        let threads: Vec<_> = (0..4)
            .map(|worker| {
                let host = host.clone();
                std::thread::spawn(move || {
                    for round in 0..200u32 {
                        let batch = (0..2)
                            .map(|screen| NewArtifact {
                                bytes: format!("{worker}-{round}-{screen}").into_bytes(),
                                screen: "top".into(),
                                width: 0,
                                height: 0,
                                mime_type: "application/octet-stream".into(),
                                extension: "bgra",
                            })
                            .collect();
                        host.publish(&format!("s{worker}"), batch).expect("publish");
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("publisher");
        }
        let listed = host.artifacts();
        assert!(listed.len() <= 16);
        for artifact in &listed {
            let bytes = host
                .read_artifact(&artifact.uri)
                .expect("catalogued file readable");
            assert_eq!(bytes.len(), artifact.bytes);
        }
        let files = fs::read_dir(base.join("store/artifacts"))
            .expect("dir")
            .count();
        assert_eq!(files, listed.len(), "no orphaned or temporary files");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn invalid_session_limits_are_rejected() {
        let mut config =
            HostConfig::new(std::env::current_exe().expect("exe"), std::env::temp_dir());
        config.max_sessions = MAX_SESSIONS_LIMIT + 1;
        assert!(Supervisor::new(config).is_err());
    }
}
