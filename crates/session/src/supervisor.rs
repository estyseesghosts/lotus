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
}

/// A session's lock domain. Lock order: `native`, then `info` or `telemetry`.
struct SessionHandle {
    info: Mutex<SessionInfo>,
    root: PathBuf,
    native: tokio::sync::Mutex<NativeSession>,
    telemetry: Mutex<Telemetry>,
    playing: AtomicBool,
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
            let mut native = NativeSession::spawn(&self.config.worker, &root).await?;
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
        });
        reservation.commit(handle);
        Ok(info)
    }

    /// Load approved content, replacing what was loaded. Leaves the session paused.
    pub async fn load(&self, id: &str, asset: &str, actor: Actor) -> io::Result<SessionInfo> {
        self.require_control()?;
        let content = self.resolve_asset(asset)?;
        let handle = self.handle(id)?;
        handle.set_playing(false);
        let mut native = handle.native.lock().await;
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
        handle.set_playing(false);
        let mut native = handle.native.lock().await;
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
        handle.set_playing(false);
        let mut native = handle.native.lock().await;
        pause_if_running(&mut native).await?;
        handle.record(actor, &format!("run_until {frames}"));
        native.advance(frames).await
    }

    pub async fn reset(&self, id: &str, actor: Actor) -> io::Result<serde_json::Value> {
        self.require_control()?;
        let handle = self.handle(id)?;
        handle.set_playing(false);
        let mut native = handle.native.lock().await;
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
        if actor != Actor::Desktop {
            handle.record(actor, "input");
        }
        native.input(state, owner).await
    }

    /// End a session. Secondary guest data is deleted; primary data persists.
    pub async fn stop(&self, id: &str, actor: Actor) -> io::Result<()> {
        self.require_control()?;
        let handle = {
            let mut registry = lock(&self.registry);
            let handle = registry
                .sessions
                .remove(id)
                .ok_or_else(|| io::Error::other("unknown session"))?;
            registry.mounted.retain(|_, owner| owner != id);
            handle
        };
        handle.set_playing(false);
        handle.record(actor, "stop");
        let mut native = handle.native.lock().await;
        let result = if native.state().is_active() {
            native.stop().await
        } else {
            Ok(())
        };
        let end = if result.is_ok() && native.state() == SessionState::Stopped {
            ScratchEnd::Clean
        } else {
            ScratchEnd::Abnormal
        };
        drop(native);
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

    /// Latest screens for display, or `None` when the session is busy or blank.
    pub async fn preview(&self, id: &str) -> Option<Vec<Frame>> {
        let handle = self.handle(id).ok()?;
        let mut native = handle.native.try_lock().ok()?;
        let has_frames = handle.telemetry().frames > 0;
        if !matches!(native.state(), SessionState::Running | SessionState::Paused) || !has_frames {
            return None;
        }
        native.capture().await.ok()
    }

    pub async fn capture(&self, id: &str) -> io::Result<Vec<Artifact>> {
        if lock(&self.artifacts).len() + 2 > MAX_ARTIFACTS {
            return Err(io::Error::other(
                "artifact quota reached; restart host to reset catalog",
            ));
        }
        let handle = self.handle(id)?;
        let frames = handle.native.lock().await.capture().await?;
        let directory = self.config.storage_root.join("artifacts");
        fs::create_dir_all(&directory)?;
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
            };
            lock(&self.artifacts).insert(artifact.uri.clone(), (artifact.clone(), path));
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
            let _ = tokio::time::timeout(CLOSE_DEADLINE, self.stop(&id, Actor::Host)).await;
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
                    "session is not owned by this host",
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
