//! Bridge between GPUI and the shared supervisor; no emulation on the UI thread.
//!
//! A background thread runs a Tokio runtime. Its main loop only drains UI
//! commands (each its own task) and publishes state every 33 ms; it never awaits
//! a worker. The selected session's preview, inspector polling and audio focus
//! run as separate tasks whose stale results are dropped (#21). Gauges,
//! continuous running and live input belong to each session's own tasks in the
//! supervisor; the window submits input without waiting. With [`McpMode::Stdio`] the same runtime also serves the
//! MCP tools over stdio against the same supervisor, so an agent and the person
//! at the window drive one set of sessions.
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::inspection::{Inspection, Panel};
use crate::presence::{self, Presence};
use lotus_mcp::server::{Frontend, LotusServer};
use lotus_protocol::debug::DebugCommand;
use lotus_session::native_session::{Frame, InputState};
use lotus_session::supervisor::{HostConfig, SessionKind, SessionSummary, Supervisor};
use lotus_session::telemetry::Actor;
use lotus_session::SessionState;
use rmcp::ServiceExt;
use serde_json::Value;

/// Commands from the window. Each completes asynchronously.
#[derive(Debug, Clone)]
pub enum Command {
    Create {
        kind: SessionKind,
        asset: Option<String>,
    },
    Load {
        session: String,
        asset: String,
    },
    /// A file the person picked; approved for this host, then loaded.
    /// Without a session, a new parentless secondary is created for it.
    OpenFile {
        session: Option<String>,
        path: PathBuf,
    },
    Select(String),
    Debug {
        session: String,
        command: DebugCommand,
    },
    OpenTrace {
        path: PathBuf,
    },
    Play(String),
    Pause(String),
    /// Advance one frame and stay paused.
    Step(String),
    Reset(String),
    Stop(String),
    Abort(String),
}

/// Whether the window also serves MCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMode {
    Off,
    Stdio,
}

/// MCP link as shown in the window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpLink {
    #[default]
    Off,
    Attached,
    Closed,
}

/// Newest screens of one session; `sequence` changes with every capture.
pub struct Preview {
    pub session: String,
    pub sequence: u64,
    pub frames: Vec<Frame>,
}

pub struct DebugImage {
    pub session: String,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// Latest published state. The window takes `preview` when it changes.
#[derive(Default)]
pub struct Update {
    pub sessions: Vec<SessionSummary>,
    pub cards: Vec<String>,
    pub assets: Vec<String>,
    pub max_sessions: usize,
    pub control: bool,
    pub selected: String,
    pub preview: Option<Preview>,
    /// Registers of the selected session at its last paused frame.
    pub registers: Option<(String, Value)>,
    pub inspection: Option<(String, Panel, Arc<Inspection>)>,
    pub debug_image: Option<DebugImage>,
    pub error: String,
    /// Worker operations of this window pending longer than one second.
    pub waiting: Vec<String>,
    pub mcp: McpLink,
    /// Commands still in flight.
    pub pending: usize,
}

pub struct DesktopController {
    pub commands: SyncSender<Command>,
    pub latest: Arc<Mutex<Update>>,
    /// Fetch registers while paused only when the inspector is open.
    pub inspector: Arc<AtomicBool>,
    pub inspection_selection: Arc<Mutex<(Panel, u32)>>,
    /// Discord Rich Presence; configured from the window's preferences.
    pub presence: Arc<Presence>,
    host: Arc<Supervisor>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

const TICK: Duration = Duration::from_millis(33);
const CATALOG_PERIOD: Duration = Duration::from_secs(1);
const INSPECT_PERIOD: Duration = Duration::from_millis(500);
/// An operation pending this long is shown as waiting.
const SLOW: Duration = Duration::from_secs(1);

/// A 33 ms ticker on fixed deadlines. `sleep(TICK)` measured 46.6 ms per loop on Windows,
/// whose default timer resolution rounds each sleep up; deadlines keep the average period.
/// A late tick is skipped rather than replayed in a burst.
fn tick() -> tokio::time::Interval {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker
}

/// Host latency of one kind of desktop worker operation.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct OperationStats {
    pub count: u64,
    pub last_ms: f64,
    pub max_ms: f64,
}

/// Desktop loop cadence and per-operation latency, published to `lotus_status`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LoopMetrics {
    /// State publications to the window.
    pub ticks: u64,
    /// Interval between the last two publications; the target is 33 ms.
    pub tick_interval_ms: f64,
    pub tick_interval_max_ms: f64,
    pub preview: OperationStats,
    pub registers: OperationStats,
    pub inspection: OperationStats,
    pub audio_focus: OperationStats,
    /// Results discarded because the selection, panel or epoch changed meanwhile.
    pub stale_dropped: u64,
}

impl DesktopController {
    pub fn start(config: HostConfig, mcp: McpMode) -> io::Result<Self> {
        let host = Arc::new(Supervisor::new(config)?);
        let (sender, receiver) = mpsc::sync_channel::<Command>(32);
        let latest = Arc::new(Mutex::new(Update {
            max_sessions: host.max_sessions(),
            control: host.allows_control(),
            ..Update::default()
        }));
        let inspector = Arc::new(AtomicBool::new(false));
        let inspection_selection = Arc::new(Mutex::new((Panel::Cpu, 0)));
        let shutdown = Arc::new(AtomicBool::new(false));
        let presence = Arc::new(Presence::start());
        let context = Loops {
            presence: presence.clone(),
            host: host.clone(),
            published: latest.clone(),
            inspector: inspector.clone(),
            inspection_selection: inspection_selection.clone(),
            previous_draw: Arc::default(),
            shutdown: shutdown.clone(),
            pending: Arc::new(AtomicUsize::new(0)),
            selected: Arc::new(tokio::sync::watch::channel(String::new()).0),
            generation: Arc::new(AtomicU64::new(0)),
            in_flight: Arc::default(),
            metrics: Arc::default(),
        };
        let thread = std::thread::Builder::new()
            .name("lotus-supervisor".into())
            .spawn(move || {
                let runtime = tokio::runtime::Runtime::new().expect("supervisor runtime");
                runtime.block_on(context.run(receiver, mcp));
            })?;
        Ok(Self {
            commands: sender,
            latest,
            inspector,
            inspection_selection,
            presence,
            host,
            shutdown,
            thread: Some(thread),
        })
    }

    /// Queue a complete live input state for `session`; never waits on the worker.
    pub fn submit_input(&self, session: &str, state: InputState) {
        let _ = self
            .host
            .submit_input(session, state, "live", Actor::Desktop);
    }

    /// The supervisor shared with MCP, for tests and embedding.
    #[must_use]
    pub fn host(&self) -> &Arc<Supervisor> {
        &self.host
    }
}

impl Drop for DesktopController {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
struct Loops {
    host: Arc<Supervisor>,
    presence: Arc<Presence>,
    published: Arc<Mutex<Update>>,
    inspector: Arc<AtomicBool>,
    inspection_selection: Arc<Mutex<(Panel, u32)>>,
    previous_draw: Arc<Mutex<Option<(String, Value)>>>,
    shutdown: Arc<AtomicBool>,
    pending: Arc<AtomicUsize>,
    /// The selected session, for the preview, inspector and audio tasks.
    selected: Arc<tokio::sync::watch::Sender<String>>,
    /// Increments on every selection change; results carry the value they started with.
    generation: Arc<AtomicU64>,
    /// Operation kind -> (session, start), for [`Update::waiting`].
    in_flight: Arc<Mutex<BTreeMap<&'static str, (String, Instant)>>>,
    metrics: Arc<Mutex<LoopMetrics>>,
}

impl Loops {
    /// Publishes state every tick without awaiting any worker. Preview, inspector polling
    /// and audio focus run as their own tasks; a slow worker delays only its own results.
    async fn run(self, receiver: mpsc::Receiver<Command>, mcp: McpMode) {
        let mut background = Vec::new();
        if mcp == McpMode::Stdio {
            background.push(tokio::spawn(self.clone().serve_mcp()));
        }
        background.push(tokio::spawn(self.clone().preview_loop()));
        background.push(tokio::spawn(self.clone().inspector_loop()));
        background.push(tokio::spawn(self.clone().audio_focus_loop()));
        let mut catalog_at: Option<Instant> = None;
        let mut published_at: Option<Instant> = None;
        let mut ticker = tick();
        // Handle -> (canonical path, file name); resolved once per handle.
        let mut content_names: BTreeMap<String, Option<(String, String)>> = BTreeMap::new();
        while !self.shutdown.load(Ordering::Acquire) {
            if !self.drain_commands(&receiver) {
                break;
            }
            if let Some(id) = self.host.take_focus_request() {
                lock(&self.published).selected = id;
            }
            if catalog_at.is_none_or(|at| at.elapsed() >= CATALOG_PERIOD) {
                let cards = self.host.cards().unwrap_or_default();
                let assets = self.host.assets();
                let mut update = lock(&self.published);
                update.cards = cards;
                update.assets = assets;
                catalog_at = Some(Instant::now());
            }
            let sessions = self.host.summaries();
            self.presence
                .publish(presence::live_content(&sessions, |handle| {
                    content_names
                        .entry(handle.to_string())
                        .or_insert_with(|| {
                            let path = self.host.asset_path(handle)?;
                            let name = path.file_name()?.to_string_lossy().into_owned();
                            Some((path.to_string_lossy().into_owned(), name))
                        })
                        .clone()
                }));
            let waiting = self.waiting();
            let selected = {
                let mut update = lock(&self.published);
                if !sessions.iter().any(|s| s.info.id == update.selected) {
                    update.selected = sessions
                        .first()
                        .map(|s| s.info.id.clone())
                        .unwrap_or_default();
                }
                update.sessions = sessions;
                update.pending = self.pending.load(Ordering::Acquire);
                update.waiting = waiting;
                update.selected.clone()
            };
            self.selected.send_if_modified(|current| {
                let changed = *current != selected;
                if changed {
                    current.clone_from(&selected);
                    self.generation.fetch_add(1, Ordering::AcqRel);
                }
                changed
            });
            let now = Instant::now();
            if let Some(previous) = published_at {
                let interval = now.duration_since(previous).as_secs_f64() * 1e3;
                let mut metrics = lock(&self.metrics);
                metrics.ticks += 1;
                metrics.tick_interval_ms = interval;
                metrics.tick_interval_max_ms = metrics.tick_interval_max_ms.max(interval);
                self.host
                    .set_frontend_metrics(serde_json::to_value(&*metrics).unwrap_or_default());
            }
            published_at = Some(now);
            ticker.tick().await;
        }
        for task in background {
            task.abort();
        }
        self.host.close().await;
    }

    /// Operations older than [`SLOW`], for the toolbar instead of a frozen window.
    fn waiting(&self) -> Vec<String> {
        lock(&self.in_flight)
            .iter()
            .filter(|(_, (_, started))| started.elapsed() >= SLOW)
            .map(|(kind, (session, started))| {
                let short = session.get(..8).unwrap_or(session);
                format!("{kind} on {short} {:.1} s", started.elapsed().as_secs_f64())
            })
            .collect()
    }

    /// Run one worker operation, recording its host latency and making it visible while slow.
    async fn timed<T>(
        &self,
        kind: &'static str,
        session: &str,
        operation: impl std::future::Future<Output = T>,
    ) -> T {
        let started = Instant::now();
        lock(&self.in_flight).insert(kind, (session.to_string(), started));
        let output = operation.await;
        lock(&self.in_flight).remove(kind);
        let ms = started.elapsed().as_secs_f64() * 1e3;
        let mut metrics = lock(&self.metrics);
        let stats = match kind {
            "preview" => &mut metrics.preview,
            "registers" => &mut metrics.registers,
            "audio focus" => &mut metrics.audio_focus,
            _ => &mut metrics.inspection,
        };
        stats.count += 1;
        stats.last_ms = ms;
        stats.max_ms = stats.max_ms.max(ms);
        output
    }

    /// Whether a result requested at `generation` for `session` still matches the view.
    fn current(&self, session: &str, generation: u64) -> bool {
        let current = *self.selected.borrow() == session
            && self.generation.load(Ordering::Acquire) == generation;
        if !current {
            lock(&self.metrics).stale_dropped += 1;
        }
        current
    }

    /// Live screens of the selected session only. An exchange already in flight is never
    /// cancelled (dropping it would leave its reply on the pipe); a result for a session or
    /// epoch that is no longer shown is discarded instead.
    async fn preview_loop(self) {
        let mut previewed = String::new();
        let mut sequence = 0u64;
        let mut ticker = tick();
        loop {
            ticker.tick().await;
            let selected = self.selected.borrow().clone();
            let generation = self.generation.load(Ordering::Acquire);
            if selected.is_empty() {
                continue;
            }
            if previewed != selected {
                // The window drops its images on a session switch (lotus_window.rs).
                self.host.forget_preview(&selected);
                previewed.clone_from(&selected);
            }
            let Some(frames) = self
                .timed("preview", &selected, self.host.preview(&selected))
                .await
            else {
                continue;
            };
            let epoch = frames.first().map_or(0, |frame| frame.lease.epoch);
            let newer_epoch = self
                .host
                .summary(&selected)
                .is_ok_and(|summary| summary.telemetry.epoch > epoch);
            if !self.current(&selected, generation) || newer_epoch {
                continue;
            }
            sequence += 1;
            lock(&self.published).preview = Some(Preview {
                session: selected,
                sequence,
                frames,
            });
        }
    }

    /// Inspector data for the selected session while the inspector is open. Live panels
    /// refresh every 500 ms; on-change panels ([`Panel::live`]) only when the view or the
    /// paused frame changes. One request at a time, so a slow worker never piles them up.
    async fn inspector_loop(self) {
        let mut registers_key = None;
        let mut inspection_key = None;
        let mut inspected_at = Instant::now();
        let mut ticker = tick();
        loop {
            ticker.tick().await;
            if !self.inspector.load(Ordering::Acquire) {
                continue;
            }
            let selected = self.selected.borrow().clone();
            let generation = self.generation.load(Ordering::Acquire);
            let Ok(summary) = self.host.summary(&selected) else {
                continue;
            };
            let telemetry = &summary.telemetry;
            let paused = telemetry.state == SessionState::Paused && !telemetry.playing;
            let key = paused.then(|| (selected.clone(), telemetry.epoch, telemetry.frames));
            if key.is_some() && key != registers_key {
                let result = self
                    .timed("registers", &selected, self.host.registers(&selected, 0))
                    .await;
                if let Ok(value) = result {
                    if self.current(&selected, generation) {
                        lock(&self.published).registers = Some((selected.clone(), value));
                        registers_key.clone_from(&key);
                    }
                }
            } else if key.is_none() {
                registers_key = None;
            }
            let (panel, offset) = *lock(&self.inspection_selection);
            let view_key = (
                selected.clone(),
                panel,
                offset,
                (telemetry.epoch, telemetry.frames),
            );
            let due = match &inspection_key {
                None => true,
                Some((session, old_panel, old_offset, frame)) => {
                    (session, old_panel, old_offset) != (&selected, &panel, &offset)
                        || if panel.live() {
                            inspected_at.elapsed() >= INSPECT_PERIOD
                        } else {
                            paused && *frame != view_key.3
                        }
                }
            };
            if !due {
                continue;
            }
            let Some(command) = panel.command(offset) else {
                inspection_key = Some(view_key);
                continue;
            };
            let value = self
                .timed(
                    "inspection",
                    &selected,
                    self.inspect(&selected, panel, &command),
                )
                .await;
            inspected_at = Instant::now();
            if self.current(&selected, generation)
                && *lock(&self.inspection_selection) == (panel, offset)
            {
                lock(&self.published).inspection = Some((
                    selected,
                    panel,
                    Arc::new(Inspection::from_value(&value, panel)),
                ));
                inspection_key = Some(view_key);
            }
        }
    }

    async fn inspect(&self, selected: &str, panel: Panel, command: &DebugCommand) -> Value {
        let mut value = match self.host.debug(selected, command, Actor::Desktop).await {
            Ok(value) => value,
            Err(error) => serde_json::json!({"error":error.to_string()}),
        };
        if panel == Panel::Cpu {
            if let Some(pc) = value["registers"][15].as_u64() {
                let command = DebugCommand::Disassemble {
                    address: pc as u32,
                    length: 64,
                    thumb: value["cpsr"].as_u64().unwrap_or(0) & 32 != 0,
                };
                if let Ok(assembly) = self.host.debug(selected, &command, Actor::Desktop).await {
                    value["disassembly"] = assembly;
                }
            }
        }
        value
    }

    /// Exclusive audio follows the selection, latest selection wins. Runs apart from the UI
    /// tick, so a slow hand-off between two workers never delays publication.
    async fn audio_focus_loop(self) {
        if !self.host.allows_control() {
            return;
        }
        let mut selection = self.selected.subscribe();
        let mut focused = String::new();
        loop {
            let target = selection.borrow_and_update().clone();
            if !target.is_empty() && target != focused {
                let result = self
                    .timed("audio focus", &target, self.host.focus_audio(&target, true))
                    .await;
                if result.is_ok() {
                    focused = target;
                    continue;
                }
                // Retry a failed hand-off unless the selection moves on first.
                tokio::select! {
                    _ = selection.changed() => {}
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
                continue;
            }
            if selection.changed().await.is_err() {
                return;
            }
        }
    }

    /// Returns false once the window side has gone away.
    fn drain_commands(&self, receiver: &mpsc::Receiver<Command>) -> bool {
        for _ in 0..32 {
            let command = match receiver.try_recv() {
                Ok(command) => command,
                Err(TryRecvError::Empty) => return true,
                Err(TryRecvError::Disconnected) => return false,
            };
            if let Command::Select(id) = command {
                lock(&self.published).selected = id;
                continue;
            }
            self.pending.fetch_add(1, Ordering::AcqRel);
            let loops = self.clone();
            tokio::spawn(async move {
                let result = loops.execute(command).await;
                let mut update = lock(&loops.published);
                match result {
                    Ok(()) => update.error.clear(),
                    Err(error) => update.error = error.to_string(),
                }
                loops.pending.fetch_sub(1, Ordering::AcqRel);
            });
        }
        true
    }

    async fn execute(&self, command: Command) -> io::Result<()> {
        let host = &self.host;
        let actor = Actor::Desktop;
        match command {
            Command::Create { kind, asset } => {
                let info = host.create(kind, asset.as_deref(), actor).await?;
                lock(&self.published).selected = info.id.clone();
                if info.asset.is_some() {
                    host.run(&info.id, actor).await?;
                }
            }
            Command::Load { session, asset } => {
                host.load(&session, &asset, actor).await?;
                host.run(&session, actor).await?;
            }
            Command::OpenFile { session, path } => {
                let asset = host.approve_local_asset(&path)?;
                let session = match session {
                    Some(session) => {
                        host.load(&session, &asset, actor).await?;
                        session
                    }
                    None => {
                        let kind = SessionKind::Secondary { parent: None };
                        host.create(kind, Some(&asset), actor).await?.id
                    }
                };
                lock(&self.published).selected = session.clone();
                host.run(&session, actor).await?;
            }
            Command::Select(_) => {}
            Command::Debug { session, command } => {
                let mut value = host.debug(&session, &command, actor).await?;
                if matches!(
                    command,
                    DebugCommand::GpuDraw { .. } | DebugCommand::ShaderTrace { .. }
                ) {
                    if let Some(uri) = value["artifacts"][0]["uri"].as_str() {
                        let bytes = host.read_artifact(uri)?;
                        if bytes.len() > 16 << 20 {
                            return Err(io::Error::other("desktop GPU snapshot exceeds 16 MiB"));
                        }
                        let capture: Value =
                            serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                        if matches!(command, DebugCommand::GpuDraw { .. }) {
                            let mut previous = lock(&self.previous_draw);
                            if let Some((id, before)) =
                                previous.as_ref().filter(|(id, _)| id == &session)
                            {
                                let _ = id;
                                value["draw_changes"] =
                                    serde_json::to_value(Inspection::draw_diff(before, &capture))
                                        .map_err(io::Error::other)?;
                            }
                            *previous = Some((session.clone(), capture.clone()));
                        }
                        value["capture"] = capture;
                    }
                }
                if matches!(command, DebugCommand::Surface { .. }) {
                    if let (Some(uri), Some(width), Some(height)) = (
                        value["artifacts"][0]["uri"].as_str(),
                        value["width"].as_u64(),
                        value["height"].as_u64(),
                    ) {
                        let bytes = host.read_artifact(uri)?;
                        if width > 1024
                            || height > 1024
                            || bytes.len() != (width * height * 4) as usize
                        {
                            return Err(io::Error::other("invalid surface image bounds"));
                        }
                        lock(&self.published).debug_image = Some(DebugImage {
                            session: session.clone(),
                            width: width as u32,
                            height: height as u32,
                            bgra: bytes,
                        });
                    }
                }
                let view = Arc::new(Inspection::from_value(&value, Panel::Result));
                lock(&self.published).inspection = Some((session, Panel::Result, view));
            }
            Command::OpenTrace { path } => {
                let view = tokio::task::spawn_blocking(move || Inspection::load_trace(&path))
                    .await
                    .map_err(io::Error::other)??;
                let mut update = lock(&self.published);
                let selected = update.selected.clone();
                update.inspection = Some((selected, Panel::Result, Arc::new(view)));
            }
            Command::Play(id) => host.run(&id, actor).await?,
            Command::Pause(id) => {
                host.pause(&id, actor).await?;
            }
            Command::Step(id) => {
                host.advance(&id, 1, actor).await?;
            }
            Command::Reset(id) => {
                host.reset(&id, actor).await?;
            }
            Command::Stop(id) => host.stop(&id, actor).await?,
            Command::Abort(id) => host.abort(&id, actor).await?,
        }
        Ok(())
    }

    async fn serve_mcp(self) {
        let server = LotusServer::shared(self.host.clone(), Frontend::Windowed, BTreeMap::new());
        let link = match server.serve(rmcp::transport::stdio()).await {
            Ok(service) => {
                lock(&self.published).mcp = McpLink::Attached;
                let _ = service.waiting().await;
                McpLink::Closed
            }
            Err(error) => {
                lock(&self.published).error = format!("MCP: {error}");
                McpLink::Closed
            }
        };
        lock(&self.published).mcp = link;
    }
}

/// Published state is plain data replaced whole; recover rather than cascade.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
