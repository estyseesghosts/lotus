//! Local host policy shared by the headless MCP server and the desktop.
//!
//! One `Supervisor` owns every session of a LOTUS process. Each session has its
//! own async lock, so a long bounded run on one session never blocks commands,
//! status or previews of another. The registry lock is never held across an
//! await. Desktop and MCP call the same methods; there is one control model.
//!
//! Sessions are *primary* (mount a persistent card) or *secondary* (private
//! scratch copy, discarded on end); see [`crate::card_store`].
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lotus_protocol::envelope::SessionUuid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;

use crate::card_store::{CardStore, ScratchEnd};
use crate::native_session::{Frame, InputState, NativeSession};
use crate::telemetry::{Actor, Telemetry};
use crate::SessionState;
mod experiments;

/// Sessions per host unless the configuration raises it; four is the tested load.
pub const DEFAULT_MAX_SESSIONS: usize = 4;
/// Upper bound accepted for `max_sessions`.
pub const MAX_SESSIONS_LIMIT: usize = 16;
pub const MAX_ARTIFACTS: usize = 128;
/// Stop deadline per worker when the host closes.
const CLOSE_DEADLINE: Duration = Duration::from_secs(3);

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
        }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub uri: String,
    pub screen: String,
    pub width: u32,
    pub height: u32,
    pub bytes: usize,
    pub sha256: String,
    pub mime_type: String,
}

/// A session's lock domain. Lock order: `native`, then `info` or `telemetry`.
struct SessionHandle {
    info: Mutex<SessionInfo>,
    root: PathBuf,
    native: tokio::sync::Mutex<NativeSession>,
    telemetry: Mutex<Telemetry>,
    playing: AtomicBool,
    lease: Mutex<Option<String>>,
    symbols: Mutex<Option<lotus_debugger::symbols::Symbols>>,
    /// (epoch, frames) of the last preview served while paused; see [`Supervisor::preview`].
    previewed: Mutex<Option<(u64, u64)>>,
}

impl SessionHandle {
    fn info(&self) -> SessionInfo {
        lock(&self.info).clone()
    }
    fn telemetry(&self) -> MutexGuard<'_, Telemetry> {
        lock(&self.telemetry)
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
}

#[derive(Default)]
struct Registry {
    sessions: BTreeMap<String, Arc<SessionHandle>>,
    /// Creations in progress, counted against the session limit.
    pending: usize,
    /// Card name to mounting session (or pending creation) ID.
    mounted: BTreeMap<String, String>,
}

/// No shell execution, arbitrary caller paths or remote listener.
pub struct Supervisor {
    config: HostConfig,
    cards: CardStore,
    registry: Mutex<Registry>,
    artifacts: Mutex<BTreeMap<String, (Artifact, PathBuf)>>,
    /// Content approved interactively by the desktop user.
    local_assets: Mutex<BTreeMap<String, PathBuf>>,
    focus_request: Mutex<Option<String>>,
    audio_focus: tokio::sync::Mutex<Option<String>>,
}

impl Supervisor {
    pub fn new(mut config: HostConfig) -> io::Result<Self> {
        if !(1..=MAX_SESSIONS_LIMIT).contains(&config.max_sessions) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("max_sessions must be in 1..={MAX_SESSIONS_LIMIT}"),
            ));
        }
        config.worker = config.worker.canonicalize()?;
        for worker in config.workers.values_mut() {
            *worker = worker.canonicalize()?;
        }
        for path in config.assets.values_mut() {
            *path = path.canonicalize()?;
        }
        for (handle, path) in &mut config.asset_directories {
            if handle.is_empty() || handle.contains(['/', '\\']) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "asset directory handles cannot be empty or contain separators",
                ));
            }
            *path = path.canonicalize()?;
        }
        fs::create_dir_all(&config.storage_root)?;
        config.storage_root = config.storage_root.canonicalize()?;
        let cards = CardStore::open(&config.storage_root)?;
        Ok(Self {
            config,
            cards,
            registry: Mutex::default(),
            artifacts: Mutex::default(),
            local_assets: Mutex::default(),
            focus_request: Mutex::default(),
            audio_focus: tokio::sync::Mutex::default(),
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

    /// Persistent card names.
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
            let Ok(entries) = fs::read_dir(directory) else {
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
        let reservation = self.reserve(&id, &kind)?;
        let root = match &kind {
            SessionKind::Primary { card } => self.cards.primary_root(card)?,
            SessionKind::Secondary { parent } => {
                self.cards.create_scratch(&id, parent.as_deref())?
            }
        };
        let started = async {
            if let Some((source, _)) = seed {
                crate::card_store::copy_checkpoint_root(source, &root)?;
            }
            let mut native =
                NativeSession::spawn(worker.unwrap_or(&self.config.worker), &root).await?;
            let profile = seed.map_or(&self.config.profile, |(_, profile)| profile);
            if let Err(error) = native.apply_profile(profile).await {
                let _ = native.stop().await;
                return Err(error);
            }
            if seed.is_none()
                && self.config.prefer_vulkan
                && native.has_settings()
                && !profile.contains_key("graphics_api")
            {
                if let Err(error) = native.prefer_vulkan().await {
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
        telemetry.pid = native.pid();
        telemetry.record(actor, "create");
        let handle = Arc::new(SessionHandle {
            info: Mutex::new(info.clone()),
            root,
            native: tokio::sync::Mutex::new(native),
            telemetry: Mutex::new(telemetry),
            playing: AtomicBool::new(false),
            lease: Mutex::new(None),
            symbols: Mutex::new(None),
            previewed: Mutex::new(None),
        });
        reservation.commit(handle);
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
        native.load(&content).await?;
        lock(&handle.info).asset = Some(asset.to_string());
        handle.record(actor, "load");
        Ok(handle.info())
    }

    /// Run continuously: the pump re-issues bounded runs until paused.
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
        native.reset().await
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
        native.input(state, owner).await
    }

    /// End a session. Secondary guest data is deleted; primary data persists.
    pub async fn stop(&self, id: &str, actor: Actor) -> io::Result<()> {
        self.require_control()?;
        let handle = self.handle(id)?;
        handle.set_playing(false);
        handle.record(actor, "stop");
        let mut native = handle.native.lock().await;
        let result = if native.state().is_active() {
            match tokio::time::timeout(CLOSE_DEADLINE, native.stop()).await {
                Ok(result) => result,
                Err(_) => {
                    native.abort().await;
                    Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "worker stop deadline; terminated",
                    ))
                }
            }
        } else {
            Ok(())
        };
        let end = if result.is_ok() && native.state() == SessionState::Stopped {
            ScratchEnd::Clean
        } else {
            ScratchEnd::Abnormal
        };
        drop(native);
        {
            let mut registry = lock(&self.registry);
            registry.sessions.remove(id);
            registry.mounted.retain(|_, owner| owner != id);
        }
        if matches!(lock(&handle.info).kind, SessionKind::Secondary { .. }) {
            self.cards.discard_scratch(&handle.root, end)?;
        }
        result
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
        let handle = self.handle(id)?;
        let mut result = self.debug_inner(id, command, client).await?;
        handle.native.lock().await.decorate(&mut result);
        Ok(result)
    }

    async fn debug_inner(
        &self,
        id: &str,
        command: &lotus_protocol::debug::DebugCommand,
        client: &str,
    ) -> io::Result<serde_json::Value> {
        use lotus_protocol::debug::DebugCommand;
        command.validate().map_err(io::Error::other)?;
        if command.mutates()
            || matches!(
                command,
                DebugCommand::AudioFocus { .. } | DebugCommand::AudioCapture { .. }
            )
        {
            self.require_control()?;
        }
        let handle = self.handle(id)?;
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
                check_lease(&handle, client)?;
                native.metadata()
            };
            self.stop(id, Actor::Host).await?;
            return Ok(serde_json::json!({"state":"stopped","metadata":metadata}));
        }
        let mut native = handle.native.lock().await;
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
            self.import_debug_artifacts(&handle.root, &mut result)?;
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
            let mut lease = lock(&handle.lease);
            if owner != client || lease.as_deref() != Some(client) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "debug lease is not owned by this client",
                ));
            }
            *lease = None;
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
            let bytes: Vec<u8> =
                serde_json::from_value(result["bytes"].clone()).map_err(io::Error::other)?;
            let text = crate::disassembly::decode(&handle.root, *address, &bytes, *thumb).await?;
            let mut result = serde_json::json!({"address":address,"bytes":bytes,"thumb":thumb,"assembly":text,
                "paused":result["paused"],"safe_point":true,"rasterizer_cache_flush":true,
                "profile_hash":native.profile_hash()});
            if let Some(symbols) = lock(&handle.symbols).as_ref() {
                result["source"] =
                    serde_json::to_value(symbols.locate(*address)?).map_err(io::Error::other)?;
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
            *lock(&handle.symbols) = Some(symbols);
            return Ok(
                serde_json::json!({"symbols":count, "elf_sha256":format!("{:x}", Sha256::digest(bytes))}),
            );
        }
        if let DebugCommand::Symbolize { addresses } = command {
            let symbols = lock(&handle.symbols);
            let symbols = symbols
                .as_ref()
                .ok_or_else(|| io::Error::other("load an approved ELF build artifact first"))?;
            let locations: io::Result<Vec<_>> = addresses
                .iter()
                .map(|address| symbols.locate(*address))
                .collect();
            return Ok(serde_json::json!({"locations":locations?}));
        }
        if let DebugCommand::ResolveSymbol { query } = command {
            let symbols = lock(&handle.symbols);
            let symbols = symbols
                .as_ref()
                .ok_or_else(|| io::Error::other("load an approved ELF build artifact first"))?;
            return Ok(serde_json::json!({"addresses":symbols.resolve(query)?}));
        }
        // The native mutex serializes lease transitions with execution mutations.
        if command.mutates() {
            check_lease(&handle, client)?;
            handle.set_playing(false);
            pause_if_running(&mut native).await?;
            handle.record(Actor::Host, client);
        }
        match command {
            DebugCommand::Status => native.status().await,
            DebugCommand::Reset => native.reset().await,
            DebugCommand::Load { asset } => {
                let result = native.load(&self.resolve_asset(asset)?).await?;
                lock(&handle.info).asset = Some(asset.clone());
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
                self.import_debug_artifacts(&handle.root, &mut result)?;
                if let Some(symbols) = lock(&handle.symbols).as_ref() {
                    if let Some(frames) = result
                        .get_mut("frames")
                        .and_then(serde_json::Value::as_array_mut)
                    {
                        for frame in frames {
                            if let Some(pc) = frame["pc"].as_u64() {
                                frame["source"] = serde_json::to_value(symbols.locate(pc as u32)?)
                                    .map_err(io::Error::other)?;
                            }
                        }
                    }
                }
                Ok(result)
            }
        }
    }

    fn import_debug_artifacts(
        &self,
        root: &Path,
        result: &mut serde_json::Value,
    ) -> io::Result<()> {
        let Some(files) = result["artifacts"].as_array() else {
            return Ok(());
        };
        if files.len() > 16 {
            return Err(io::Error::other("artifact batch exceeds budget"));
        }
        let root = root.canonicalize()?;
        let mut catalog = lock(&self.artifacts);
        if catalog.len() + files.len() > MAX_ARTIFACTS {
            return Err(io::Error::other("artifact quota reached"));
        }
        let directory = self.config.storage_root.join("artifacts");
        fs::create_dir_all(&directory)?;
        let mut published = Vec::new();
        let mut total: u64 = catalog.values().map(|(a, _)| a.bytes as u64).sum();
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
            let size = fs::metadata(&path)?.len();
            if size > 64 << 20 || total + size > 512 << 20 {
                return Err(io::Error::other("artifact byte quota reached"));
            }
            total += size;
            let bytes = fs::read(&path)?;
            let id = SessionUuid::generate().to_string();
            let destination = directory.join(&id);
            fs::write(&destination, &bytes)?;
            let artifact = Artifact {
                uri: format!("lotus://artifact/{id}"),
                screen: file["format"].as_str().unwrap_or("capture").to_string(),
                width: file["width"].as_u64().unwrap_or(0) as u32,
                height: file["height"].as_u64().unwrap_or(0) as u32,
                bytes: bytes.len(),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                mime_type: match file["format"].as_str() {
                    Some("json") => "application/json",
                    Some("wav") => "audio/wav",
                    Some("citrace") => "application/x-citrace",
                    _ => "application/octet-stream",
                }
                .into(),
            };
            catalog.insert(artifact.uri.clone(), (artifact.clone(), destination));
            published.push(artifact);
        }
        result["artifacts"] = serde_json::to_value(published).map_err(io::Error::other)?;
        Ok(())
    }

    /// Host presentation policy; never pauses the guest or changes deterministic capture.
    pub async fn focus_audio(&self, id: &str, enabled: bool) -> io::Result<serde_json::Value> {
        use lotus_protocol::debug::DebugCommand;
        self.require_control()?;
        let handle = self.handle(id)?;
        let mut focus = self.audio_focus.lock().await;
        if enabled && focus.as_deref() != Some(id) {
            if let Some(previous) = focus.take() {
                if let Ok(old) = self.handle(&previous) {
                    old.native
                        .lock()
                        .await
                        .debug(&DebugCommand::AudioFocus { enabled: false })
                        .await?;
                }
            }
        }
        let result = handle
            .native
            .lock()
            .await
            .debug(&DebugCommand::AudioFocus { enabled })
            .await?;
        if enabled {
            *focus = Some(id.to_string());
        } else if focus.as_deref() == Some(id) {
            *focus = None;
        }
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
        // A paused guest presents nothing new until it runs or its epoch changes (load,
        // reset, state restore, replay). Re-capturing would rewrite both screen files, stall
        // Vulkan on its readback and make the window rebuild identical images ~30 times/s.
        let key = (epoch, frames);
        let paused = native.state() == SessionState::Paused;
        if paused && *lock(&handle.previewed) == Some(key) {
            return None;
        }
        let captured = native.capture().await.ok()?;
        *lock(&handle.previewed) = paused.then_some(key);
        Some(captured)
    }

    pub async fn capture(&self, id: &str) -> io::Result<Vec<Artifact>> {
        self.capture_scaled(id, 1).await
    }
    pub async fn capture_scaled(&self, id: &str, scale: u32) -> io::Result<Vec<Artifact>> {
        if lock(&self.artifacts).len() + 2 > MAX_ARTIFACTS {
            return Err(io::Error::other(
                "artifact quota reached; restart host to reset catalog",
            ));
        }
        let handle = self.handle(id)?;
        let frames = handle.native.lock().await.capture_scaled(scale).await?;
        let directory = self.config.storage_root.join("artifacts");
        fs::create_dir_all(&directory)?;
        let mut catalog = lock(&self.artifacts);
        let total: usize = catalog.values().map(|(a, _)| a.bytes).sum();
        let added: usize = frames.iter().map(|frame| frame.bgra.len()).sum();
        if catalog.len() + frames.len() > MAX_ARTIFACTS || total + added > 512 << 20 {
            return Err(io::Error::other("capture artifact quota reached"));
        }
        let mut artifacts = Vec::new();
        for frame in frames {
            let id = SessionUuid::generate().to_string();
            let path = directory.join(format!("{id}.bgra"));
            fs::write(&path, &frame.bgra)?;
            let artifact = Artifact {
                uri: format!("lotus://artifact/{id}"),
                screen: frame.lease.screen,
                width: frame.lease.width,
                height: frame.lease.height,
                bytes: frame.bgra.len(),
                sha256: format!("{:x}", Sha256::digest(&frame.bgra)),
                mime_type: "application/octet-stream".into(),
            };
            catalog.insert(artifact.uri.clone(), (artifact.clone(), path));
            artifacts.push(artifact);
        }
        Ok(artifacts)
    }

    #[must_use]
    pub fn artifacts(&self) -> Vec<Artifact> {
        lock(&self.artifacts)
            .values()
            .map(|(artifact, _)| artifact.clone())
            .collect()
    }

    /// Resolve only catalogued immutable captures, never interpret a URI as a path.
    pub fn read_artifact(&self, uri: &str) -> io::Result<Vec<u8>> {
        let (artifact, path) = lock(&self.artifacts).get(uri).cloned().ok_or_else(|| {
            io::Error::new(io::ErrorKind::PermissionDenied, "unknown artifact resource")
        })?;
        let data = fs::read(path)?;
        if data.len() != artifact.bytes || format!("{:x}", Sha256::digest(&data)) != artifact.sha256
        {
            return Err(io::Error::other("artifact identity mismatch"));
        }
        Ok(data)
    }

    /// Ask a windowed frontend to show `id`. Headless hosts ignore it.
    pub fn request_focus(&self, id: &str) -> io::Result<()> {
        self.handle(id)?;
        *lock(&self.focus_request) = Some(id.to_string());
        Ok(())
    }

    #[must_use]
    pub fn take_focus_request(&self) -> Option<String> {
        lock(&self.focus_request).take()
    }

    /// Refresh gauges of every idle session and keep playing sessions running.
    /// Sessions held by another command are marked busy, never waited on.
    pub async fn pump(&self) {
        let mut tasks = JoinSet::new();
        for handle in self.handles() {
            tasks.spawn(pump_session(handle));
        }
        while tasks.join_next().await.is_some() {}
    }

    /// Stop every session, bounding each stop; used when a frontend closes.
    pub async fn close(&self) {
        for id in self.sessions() {
            let _ = self.stop(&id, Actor::Host).await;
        }
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

async fn pump_session(handle: Arc<SessionHandle>) {
    let Ok(mut native) = handle.native.try_lock() else {
        handle.telemetry().busy = true;
        return;
    };
    if !native.state().is_active() {
        handle.set_playing(false);
        let mut telemetry = handle.telemetry();
        telemetry.state = native.state();
        telemetry.fps = 0.0;
        telemetry.speed = 0.0;
        telemetry.memory_bytes = None;
        return;
    }
    let status = match native.status().await {
        Ok(status) => status,
        Err(error) => {
            handle.set_playing(false);
            let mut telemetry = handle.telemetry();
            telemetry.state = native.state();
            telemetry.last_error = error.to_string();
            return;
        }
    };
    observe(&handle, &native, &status);
    if !handle.is_playing() || native.state() != SessionState::Paused {
        return;
    }
    // A bounded run ended. Resume unless the guest stopped for a reason a retry
    // cannot fix; a host deadline only means the batch ran slowly.
    let stop = status["stop"]["reason"].as_str().unwrap_or("");
    if !matches!(stop, "" | "frame") {
        handle.set_playing(false);
        return;
    }
    let reason = status["last_error"].as_str().unwrap_or("");
    if !matches!(reason, "" | "host_deadline") {
        handle.set_playing(false);
        return;
    }
    if let Err(error) = native.run().await {
        handle.set_playing(false);
        handle.telemetry().last_error = error.to_string();
    }
}

fn observe(handle: &SessionHandle, native: &NativeSession, status: &serde_json::Value) {
    let mut telemetry = handle.telemetry();
    telemetry.playing = handle.is_playing();
    telemetry.observe(status, native.state(), native.epoch(), Instant::now());
    telemetry.pid = native.pid();
    telemetry.memory_bytes = native.working_set();
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
    fn invalid_session_limits_are_rejected() {
        let mut config =
            HostConfig::new(std::env::current_exe().expect("exe"), std::env::temp_dir());
        config.max_sessions = MAX_SESSIONS_LIMIT + 1;
        assert!(Supervisor::new(config).is_err());
    }
}
