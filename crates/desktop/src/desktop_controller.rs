//! Bridge between GPUI and the shared supervisor; no emulation on the UI thread.
//!
//! A background thread runs a Tokio runtime. Its main loop only drains UI
//! commands (each its own task) and publishes state every 33 ms; it never awaits
//! a worker. Previews of every session the workspace shows (one, or two when tiled,
//! lotus#65), the selected session's inspector polling and its audio focus run as
//! separate tasks whose stale results are dropped (#21), with at most one
//! request per operation and session in flight, so a stalled worker never delays
//! another session's (#45). Gauges,
//! continuous running and live input belong to each session's own tasks in the
//! supervisor; the window submits input without waiting. With [`McpMode::Stdio`] the same runtime also serves the
//! MCP tools over stdio against the same supervisor, so an agent and the person
//! at the window drive one set of sessions.
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::inspection::{
    self, Inspection, InspectionSelection, InspectionSnapshot, Panel, SessionTarget,
};
use crate::presence::{self, Presence};
use crate::ui_metrics::UiMetrics;
use lotus_mcp::server::{Frontend, LotusServer};
use lotus_protocol::debug::DebugCommand;
use lotus_session::native_session::Frame;
use lotus_session::supervisor::{
    debug_requires_control, DebugCancellation, HostConfig, SessionKind, SessionSummary, Supervisor,
};
use lotus_session::telemetry::Actor;

use crate::input_routing::InputRouting;
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
    /// Point the inspector at one tile's session without selecting it (lotus#71); `None`
    /// returns it to the selection. Ignored unless that session is shown in a split.
    Inspect(Option<String>),
    DebugAt {
        target: SessionTarget,
        panel: Panel,
        command: DebugCommand,
    },
    CancelDebug(u64),
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
    /// A layout change through the shared workspace (lotus#67), committed before it returns.
    Workspace(lotus_session::workspace::Operation),
    /// As [`Command::Workspace`], refused if the workspace is no longer at this revision
    /// (a drag-and-drop started on an older layout, lotus#68).
    WorkspaceAt(lotus_session::workspace::Operation, u64),
    /// How the window resolved `Auto` for a split (lotus#69); display only.
    WorkspaceResolved(lotus_session::workspace::Resolved),
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

#[derive(Debug)]
pub struct DebugImage {
    pub target: SessionTarget,
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

const DEBUG_IN_FLIGHT: usize = 8;
const DEBUG_HISTORY: usize = 24;
const DEBUG_PER_SESSION: usize = 2;

/// One explicit inspector action. The operation and its target survive panel switches.
#[derive(Clone, Debug)]
pub struct DebugRequest {
    pub id: u64,
    pub target: SessionTarget,
    pub panel: Panel,
    pub command: Arc<DebugCommand>,
    pub status: DebugStatus,
    pub requires_control: bool,
}

#[derive(Clone, Debug)]
pub enum DebugStatus {
    Running,
    Finished {
        view: Arc<Inspection>,
        image: Option<Arc<DebugImage>>,
    },
    Failed {
        kind: io::ErrorKind,
        message: String,
    },
    Cancelled {
        dispatched: bool,
    },
    Expired,
}

/// Latest published state. The window takes `previews` when they change.
#[derive(Default)]
pub struct Update {
    pub sessions: Vec<SessionSummary>,
    pub cards: Vec<String>,
    pub assets: Vec<String>,
    pub max_sessions: usize,
    pub control: bool,
    /// Inspector target: preview of a single view, inspector panels, audio focus. Distinct
    /// from `workspace`, which says what the window shows (lotus#67).
    pub selected: String,
    /// Session the inspector panels, registers and debug commands target: the tile asked
    /// for with [`Command::Inspect`] while it stays in a split, otherwise `selected`.
    /// Opening it never changes `selected`, audio focus or controller assignment.
    pub inspected: String,
    /// The tile session the inspector overlay was opened for; cleared by the controller
    /// once that session leaves the split, stops or the split ends (lotus#71).
    pub inspect_tile: Option<String>,
    pub workspace: lotus_session::workspace::Snapshot,
    /// Newest preview of each session the workspace shows, replaced whole per session.
    /// Sessions no longer shown are removed. Independent per session (lotus#65).
    pub previews: BTreeMap<String, Preview>,
    /// Registers of the inspected session at its last paused frame, with provenance.
    pub registers: Option<(SessionTarget, Value)>,
    pub inspection: Option<InspectionSnapshot>,
    debug_requests: VecDeque<DebugRequest>,
    pub error: String,
    /// Worker operations of this window pending longer than one second.
    pub waiting: Vec<String>,
    /// Artifact catalogue usage against the host quota; refreshed with the catalog.
    pub artifacts: Option<lotus_session::artifacts::Usage>,
    pub mcp: McpLink,
    /// Commands still in flight.
    pub pending: usize,
}

impl Update {
    // Recheck under the publication lock after the live-host/generation check.
    fn publish_inspection(&mut self, snapshot: InspectionSnapshot) -> bool {
        if !inspection::snapshot_current(&snapshot, &self.inspected, &self.sessions) {
            return false;
        }
        self.inspection = Some(snapshot);
        true
    }

    /// A target/epoch transition invalidates cached data even before another
    /// request completes. UI consumers also validate the provenance they hold.
    fn retain_current_inspection(&mut self) {
        self.registers = self
            .registers
            .take()
            .filter(|(target, _)| target.is_current(&self.inspected, &self.sessions));
        self.inspection = self.inspection.take().filter(|snapshot| {
            inspection::snapshot_current(snapshot, &self.inspected, &self.sessions)
        });
        for request in &mut self.debug_requests {
            if request.target.resolve(&self.sessions).is_none() {
                request.status = DebugStatus::Expired;
            }
        }
    }

    pub fn debug_requests(&self) -> impl DoubleEndedIterator<Item = &DebugRequest> {
        self.debug_requests.iter()
    }

    pub fn debug_result(&self, panel: Panel) -> Option<&DebugRequest> {
        if panel == Panel::Result
            && self
                .inspection
                .as_ref()
                .is_some_and(|item| item.target.is_none())
        {
            return None;
        }
        self.debug_requests().rev().find(|request| {
            request.panel == panel
                && request.target.is_current(&self.inspected, &self.sessions)
                && !matches!(request.status, DebugStatus::Expired)
        })
    }
}

pub struct DesktopController {
    pub commands: SyncSender<Command>,
    pub latest: Arc<Mutex<Update>>,
    /// Fetch registers while paused only when the inspector is open.
    pub inspector: Arc<AtomicBool>,
    pub inspection_selection: Arc<Mutex<InspectionSelection>>,
    /// Discord Rich Presence; configured from the window's preferences.
    pub presence: Arc<Presence>,
    /// Window-side counters, published as `frontend_metrics.ui` (lotus#74).
    pub ui: Arc<UiMetrics>,
    preview_visible: Arc<AtomicBool>,
    routing: Arc<InputRouting>,
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
    /// Requests not started because the same operation on that session was still in flight.
    pub saturated: u64,
    /// Operations in flight now, with their age.
    pub pending: Vec<PendingOperation>,
    /// Sequence of the newest preview handed to the window.
    pub published_preview_sequence: u64,
}

/// One outstanding desktop worker operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PendingOperation {
    pub kind: &'static str,
    pub session: String,
    pub age_ms: f64,
}

/// Inspector refresh state, updated by the requests' own tasks.
struct InspectorKeys {
    registers: Option<(SessionTarget, u64)>,
    inspection: Option<(SessionTarget, Panel, u64, u64)>,
    inspected_at: Instant,
}

impl DesktopController {
    pub fn start(config: HostConfig, mcp: McpMode) -> io::Result<Self> {
        let host = Arc::new(Supervisor::new(config)?);
        // A desktop host has a window; previews follow what its workspace shows. The window
        // replaces these defaults with the saved ones.
        host.enable_workspace(lotus_session::workspace::SplitDefaults::default());
        let (sender, receiver) = mpsc::sync_channel::<Command>(32);
        let latest = Arc::new(Mutex::new(Update {
            max_sessions: host.max_sessions(),
            control: host.allows_control(),
            workspace: host.workspace().snapshot,
            ..Update::default()
        }));
        let inspector = Arc::new(AtomicBool::new(false));
        let inspection_selection =
            Arc::new(Mutex::new(InspectionSelection::first_page(Panel::Cpu)));
        let shutdown = Arc::new(AtomicBool::new(false));
        let presence = Arc::new(Presence::start());
        let ui = Arc::new(UiMetrics::default());
        let preview_visible = Arc::new(AtomicBool::new(true));
        let routing = InputRouting::new(host.clone());
        let controllers = Controllers::start(routing.clone(), shutdown.clone());
        let context = Loops {
            controllers,
            ui: ui.clone(),
            presence: presence.clone(),
            host: host.clone(),
            published: latest.clone(),
            inspector: inspector.clone(),
            inspection_selection: inspection_selection.clone(),
            previous_draw: Arc::default(),
            debug_sequence: Arc::default(),
            active_debug: Arc::default(),
            result_generation: Arc::default(),
            shutdown: shutdown.clone(),
            pending: Arc::new(AtomicUsize::new(0)),
            selected: Arc::new(tokio::sync::watch::channel(String::new()).0),
            reveal: Arc::new(AtomicBool::new(false)),
            inspected: Arc::new(tokio::sync::watch::channel(String::new()).0),
            generation: Arc::new(AtomicU64::new(0)),
            in_flight: Arc::default(),
            preview_sequence: Arc::default(),
            preview_visible: preview_visible.clone(),
            metrics: Arc::default(),
        };
        let thread = std::thread::Builder::new()
            .name("lotus-supervisor".into())
            .spawn(move || {
                let runtime = tokio::runtime::Runtime::new().expect("supervisor runtime");
                runtime.block_on(context.run(receiver, mcp));
            })?;
        Ok(Self {
            routing,
            commands: sender,
            latest,
            inspector,
            inspection_selection,
            presence,
            ui,
            preview_visible,
            host,
            shutdown,
            thread: Some(thread),
        })
    }

    /// Keyboard, pointer and controller routing to sessions (lotus#61).
    #[must_use]
    pub fn input(&self) -> &Arc<InputRouting> {
        &self.routing
    }

    /// The supervisor shared with MCP, for tests and embedding.
    #[must_use]
    pub fn host(&self) -> &Arc<Supervisor> {
        &self.host
    }

    pub(crate) fn set_preview_visible(&self, visible: bool) {
        self.preview_visible.store(visible, Ordering::Release);
    }

    pub(crate) fn preview_visible(&self) -> bool {
        self.preview_visible.load(Ordering::Acquire)
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

type ActiveDebug = BTreeMap<u64, (SessionTarget, DebugCancellation)>;

#[derive(Clone)]
struct Loops {
    host: Arc<Supervisor>,
    /// Physical controllers (lotus#59), routed by [`InputRouting`] (lotus#61).
    controllers: Option<Arc<Controllers>>,
    presence: Arc<Presence>,
    published: Arc<Mutex<Update>>,
    inspector: Arc<AtomicBool>,
    inspection_selection: Arc<Mutex<InspectionSelection>>,
    previous_draw: Arc<Mutex<Option<(SessionTarget, Value)>>>,
    debug_sequence: Arc<AtomicU64>,
    active_debug: Arc<Mutex<ActiveDebug>>,
    result_generation: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    pending: Arc<AtomicUsize>,
    /// The selected session, for the preview, inspector and audio tasks.
    selected: Arc<tokio::sync::watch::Sender<String>>,
    /// Set when the selection was chosen (sidebar, focus request, a new or opened session,
    /// or the fallback after the selected one stopped); [`Loops::show_selected`] then shows
    /// it once. Otherwise the committed workspace stands, so an MCP `single` is kept (#112).
    reveal: Arc<AtomicBool>,
    /// The inspected session ([`Update::inspected`]), for the inspector task.
    inspected: Arc<tokio::sync::watch::Sender<String>>,
    /// Increments on every inspected-session change; results carry the value they started with.
    generation: Arc<AtomicU64>,
    /// (operation kind, session) -> start, at most one each; for [`Update::waiting`].
    in_flight: Arc<Mutex<BTreeMap<(&'static str, String), Instant>>>,
    /// Sequence of published previews.
    preview_sequence: Arc<AtomicU64>,
    preview_visible: Arc<AtomicBool>,
    metrics: Arc<Mutex<LoopMetrics>>,
    ui: Arc<UiMetrics>,
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
                self.select(id);
            }
            if catalog_at.is_none_or(|at| at.elapsed() >= CATALOG_PERIOD) {
                let cards = self.host.cards().unwrap_or_default();
                let assets = self.host.assets();
                let artifacts = self.host.artifact_usage();
                let mut update = lock(&self.published);
                update.cards = cards;
                update.assets = assets;
                update.artifacts = Some(artifacts);
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
            let (selected, reveal) = {
                let mut update = lock(&self.published);
                if !sessions.iter().any(|s| s.info.id == update.selected) {
                    // A stopped tile collapses the split to the other one (lotus#67); keep
                    // showing that rather than replacing it with the first session.
                    let visible = self.host.workspace().snapshot.layout;
                    update.selected = visible
                        .visible()
                        .into_iter()
                        .find(|id| sessions.iter().any(|s| s.info.id == *id))
                        .map(str::to_string)
                        .or_else(|| sessions.first().map(|s| s.info.id.clone()))
                        .unwrap_or_default();
                    self.reveal.store(true, Ordering::Release);
                }
                update.sessions = sessions;
                update.pending = self.pending.load(Ordering::Acquire);
                update.waiting = waiting;
                (
                    update.selected.clone(),
                    self.reveal.swap(false, Ordering::AcqRel),
                )
            };
            let changed = self.selected.send_if_modified(|current| {
                let changed = *current != selected;
                if changed {
                    current.clone_from(&selected);
                }
                changed
            });
            if changed && !selected.is_empty() {
                self.host.set_focus(&selected);
            }
            self.show_selected(&selected, reveal);
            let inspected = {
                let mut update = lock(&self.published);
                let shown = update.workspace.layout.visible();
                let split = matches!(
                    update.workspace.layout,
                    lotus_session::workspace::Layout::Split { .. }
                );
                let valid = update.inspect_tile.as_ref().is_some_and(|tile| {
                    split
                        && shown.contains(&tile.as_str())
                        && update.sessions.iter().any(|s| s.info.id == *tile)
                });
                if !valid {
                    update.inspect_tile = None;
                }
                update.inspected = update
                    .inspect_tile
                    .clone()
                    .unwrap_or_else(|| selected.clone());
                update.retain_current_inspection();
                for (target, cancellation) in lock(&self.active_debug).values() {
                    if target.resolve(&update.sessions).is_none() {
                        cancellation.cancel();
                    }
                }
                update.inspected.clone()
            };
            self.inspected.send_if_modified(|current| {
                let changed = *current != inspected;
                if changed {
                    current.clone_from(&inspected);
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
                metrics.pending = self.pending_operations();
                metrics.published_preview_sequence = self.preview_sequence.load(Ordering::Acquire);
                let mut value = serde_json::to_value(&*metrics).unwrap_or_default();
                value["ui"] = self.ui.snapshot();
                if let Some(controllers) = &self.controllers {
                    value["controllers"] = controllers.status();
                }
                self.host.set_frontend_metrics(value);
            }
            published_at = Some(now);
            ticker.tick().await;
        }
        for (_, cancellation) in lock(&self.active_debug).values() {
            cancellation.cancel();
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
            .filter(|(_, started)| started.elapsed() >= SLOW)
            .map(|((kind, session), started)| {
                let short = session.get(..8).unwrap_or(session);
                format!("{kind} on {short} {:.1} s", started.elapsed().as_secs_f64())
            })
            .collect()
    }

    /// Every outstanding desktop operation with its age, for `frontend_metrics.pending`.
    fn pending_operations(&self) -> Vec<PendingOperation> {
        lock(&self.in_flight)
            .iter()
            .map(|((kind, session), started)| PendingOperation {
                kind,
                session: session.clone(),
                age_ms: started.elapsed().as_secs_f64() * 1e3,
            })
            .collect()
    }

    /// Claim the one slot for `kind` on `session`. False while an earlier request of that
    /// kind on that session is still in flight (counted as saturated).
    fn begin(&self, kind: &'static str, session: &str) -> bool {
        let mut in_flight = lock(&self.in_flight);
        let key = (kind, session.to_string());
        if in_flight.contains_key(&key) {
            drop(in_flight);
            lock(&self.metrics).saturated += 1;
            return false;
        }
        in_flight.insert(key, Instant::now());
        true
    }

    /// Run one worker operation claimed with [`Self::begin`], recording its host latency.
    /// The slot is released when the operation completes, however long the worker takes.
    async fn timed<T>(
        &self,
        kind: &'static str,
        session: &str,
        operation: impl std::future::Future<Output = T>,
    ) -> T {
        let started = Instant::now();
        let output = operation.await;
        lock(&self.in_flight).remove(&(kind, session.to_string()));
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

    /// Whether a result's target and request generation still match the view.
    fn current(&self, target: &SessionTarget, generation: u64) -> bool {
        let current = *self.inspected.borrow() == target.id
            && self.generation.load(Ordering::Acquire) == generation
            && self
                .host
                .summary(&target.id)
                .is_ok_and(|summary| target.resolve(std::slice::from_ref(&summary)).is_some());
        if !current {
            lock(&self.metrics).stale_dropped += 1;
        }
        current
    }

    fn current_reply(&self, target: &SessionTarget, generation: u64, value: &Value) -> bool {
        if !target.accepts_reply(value) {
            lock(&self.metrics).stale_dropped += 1;
            return false;
        }
        self.current(target, generation)
    }

    /// Sessions the workspace shows now, in tile order.
    fn visible(&self) -> Vec<String> {
        let snapshot = self.host.workspace().snapshot;
        snapshot
            .layout
            .visible()
            .into_iter()
            .map(str::to_string)
            .collect()
    }

    /// Live screens of every session the workspace shows (lotus#65). Each request runs in
    /// its own task, at most one per session (#45): a stalled worker keeps its own exchange,
    /// which is never cancelled (dropping it would leave its reply on the pipe), and never
    /// delays another tile's. Results for a session no longer shown, or for an older epoch,
    /// are discarded. No new requests are issued while the window is hidden.
    async fn preview_loop(self) {
        let mut previewed: Vec<String> = Vec::new();
        let mut ticker = tick();
        loop {
            ticker.tick().await;
            let visible = self.visible();
            for session in &visible {
                if !previewed.contains(session) {
                    // The window drops a tile's images when it stops being shown.
                    self.host.forget_preview(session);
                }
            }
            if previewed != visible {
                lock(&self.published)
                    .previews
                    .retain(|session, _| visible.contains(session));
                previewed.clone_from(&visible);
            }
            if !self.preview_visible.load(Ordering::Acquire) {
                continue;
            }
            for session in visible {
                if !self.begin("preview", &session) {
                    continue;
                }
                let loops = self.clone();
                tokio::spawn(async move {
                    let Some(frames) = loops
                        .timed("preview", &session, loops.host.preview(&session))
                        .await
                    else {
                        return;
                    };
                    let epoch = frames.first().map_or(0, |frame| frame.lease.epoch);
                    let newer_epoch = loops
                        .host
                        .summary(&session)
                        .is_ok_and(|summary| summary.telemetry.epoch > epoch);
                    if newer_epoch || !loops.visible().contains(&session) {
                        lock(&loops.metrics).stale_dropped += 1;
                        return;
                    }
                    let sequence = loops.preview_sequence.fetch_add(1, Ordering::AcqRel) + 1;
                    lock(&loops.published).previews.insert(
                        session.clone(),
                        Preview {
                            session,
                            sequence,
                            frames,
                        },
                    );
                });
            }
        }
    }

    /// Inspector data for the inspected session while the inspector is open. Live panels
    /// refresh every 500 ms; on-change panels ([`Panel::live`]) only when the view or the
    /// paused frame changes. Registers and inspection each run as their own task, one per
    /// session at a time, so a worker stalled on the previous selection never delays the
    /// new one (#45) and a slow worker never piles requests up.
    async fn inspector_loop(self) {
        let state = Arc::new(Mutex::new(InspectorKeys {
            registers: None,
            inspection: None,
            inspected_at: Instant::now(),
        }));
        let mut ticker = tick();
        loop {
            ticker.tick().await;
            if !self.inspector.load(Ordering::Acquire) {
                continue;
            }
            let selected = self.inspected.borrow().clone();
            // New inspector polls yield to explicit commands on this worker.
            if lock(&self.active_debug)
                .values()
                .any(|(target, _)| target.id == selected)
            {
                continue;
            }
            let generation = self.generation.load(Ordering::Acquire);
            let Ok(summary) = self.host.summary(&selected) else {
                continue;
            };
            let target = SessionTarget::from_summary(&summary);
            let telemetry = &summary.telemetry;
            let paused = telemetry.state == SessionState::Paused && !telemetry.playing;
            let key = paused.then(|| (target.clone(), telemetry.frames));
            let registers_due = {
                let mut keys = lock(&state);
                if key.is_none() {
                    keys.registers = None;
                }
                key.is_some() && key != keys.registers
            };
            if registers_due && self.begin("registers", &selected) {
                let (loops, state, selected, key) =
                    (self.clone(), state.clone(), selected.clone(), key.clone());
                let target = target.clone();
                tokio::spawn(async move {
                    let result = loops
                        .timed("registers", &selected, loops.host.registers(&selected, 0))
                        .await;
                    if let Ok(value) = result {
                        let mut update = lock(&loops.published);
                        if loops.current_reply(&target, generation, &value)
                            && target.is_current(&update.inspected, &update.sessions)
                        {
                            update.registers = Some((target, value));
                            lock(&state).registers = key;
                        }
                    }
                });
            }
            let selection = lock(&self.inspection_selection).clone();
            let panel = selection.panel;
            let offset = selection.offset_for(&target);
            let view_key = (target.clone(), panel, offset, telemetry.frames);
            let due = {
                let keys = lock(&state);
                match &keys.inspection {
                    None => true,
                    Some((old_target, old_panel, old_offset, frame)) => {
                        (old_target, old_panel, old_offset) != (&target, &panel, &offset)
                            || if panel.live() {
                                keys.inspected_at.elapsed() >= INSPECT_PERIOD
                            } else {
                                paused && *frame != view_key.3
                            }
                    }
                }
            };
            if !due {
                continue;
            }
            let Some(command) = panel.command(offset) else {
                lock(&state).inspection = Some(view_key);
                continue;
            };
            if !self.begin("inspection", &selected) {
                continue;
            }
            let (loops, state) = (self.clone(), state.clone());
            tokio::spawn(async move {
                let result = loops
                    .timed(
                        "inspection",
                        &selected,
                        loops.inspect(&target, panel, &command),
                    )
                    .await;
                lock(&state).inspected_at = Instant::now();
                let view = match result {
                    Ok(value) if loops.current_reply(&target, generation, &value) => {
                        Inspection::from_value(&value, panel)
                    }
                    Ok(_) => return,
                    Err(error) => Inspection::failed(error),
                };
                let mut update = lock(&loops.published);
                let current = lock(&loops.inspection_selection);
                if loops.current(&target, generation)
                    && current.panel == panel
                    && current.offset_for(&target) == offset
                    && update.publish_inspection(InspectionSnapshot {
                        target: Some(target),
                        panel,
                        offset: Some(offset),
                        view: Arc::new(view),
                    })
                {
                    lock(&state).inspection = Some(view_key);
                }
            });
        }
    }

    async fn inspect(
        &self,
        target: &SessionTarget,
        panel: Panel,
        command: &DebugCommand,
    ) -> io::Result<Value> {
        let mut value = self.dispatch_debug(target, command).await?;
        if panel == Panel::Cpu {
            if let Some(pc) = value["registers"][15].as_u64() {
                let command = DebugCommand::Disassemble {
                    address: pc as u32,
                    length: 64,
                    thumb: value["cpsr"].as_u64().unwrap_or(0) & 32 != 0,
                };
                if let Ok(assembly) = self.dispatch_debug(target, &command).await {
                    value["disassembly"] = assembly;
                }
            }
        }
        Ok(value)
    }

    /// Exclusive audio follows the selection, latest selection wins. Each hand-off runs as
    /// its own task, so one stalled worker never delays focus moving to another (#45);
    /// [`Supervisor::focus_audio`] orders hand-offs per worker. A failed hand-off is retried
    /// after a second unless the selection moves on first.
    async fn audio_focus_loop(self) {
        if !self.host.allows_control() {
            return;
        }
        let mut selection = self.selected.subscribe();
        // Last target requested and whether it has failed since.
        let requested = Arc::new(Mutex::new((String::new(), false)));
        let failed_wake = Arc::new(tokio::sync::Notify::new());
        loop {
            let target = selection.borrow_and_update().clone();
            let retry = {
                let mut requested = lock(&requested);
                let start = !target.is_empty() && (requested.0 != target || requested.1);
                if start {
                    *requested = (target.clone(), false);
                }
                start
            };
            if retry && self.begin("audio focus", &target) {
                let (loops, requested, wake) =
                    (self.clone(), requested.clone(), failed_wake.clone());
                tokio::spawn(async move {
                    let result = loops
                        .timed(
                            "audio focus",
                            &target,
                            loops.host.focus_audio(&target, true),
                        )
                        .await;
                    if result.is_err() {
                        let mut requested = lock(&requested);
                        if requested.0 == target {
                            requested.1 = true;
                            wake.notify_one();
                        }
                    }
                });
            }
            let failed = lock(&requested).1;
            tokio::select! {
                changed = selection.changed() => if changed.is_err() { return },
                // A failure: wait a second, unless the selection moves on first.
                () = failed_wake.notified(), if !failed => {
                    tokio::select! {
                        changed = selection.changed() => if changed.is_err() { return },
                        () = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                }
                () = tokio::time::sleep(Duration::from_secs(1)), if failed => {}
            }
        }
    }

    /// Selects `id` and asks [`Loops::show_selected`] to show it.
    fn select(&self, id: String) {
        // Under the same lock the loop reads both with, so it never shows a stale id.
        let mut update = lock(&self.published);
        update.selected = id;
        self.reveal.store(true, Ordering::Release);
    }

    /// Outside a split, a newly chosen selection is shown; this goes through the shared
    /// workspace operation so MCP sees the same revision. A split is never collapsed by
    /// selection, and an unchanged selection never overrides the committed workspace.
    /// A session that vanished meanwhile is refused and retried next tick.
    fn show_selected(&self, selected: &str, reveal: bool) {
        use lotus_session::workspace::{Layout, Operation};
        let status = self.host.workspace();
        if !reveal {
            lock(&self.published).workspace = status.snapshot;
            return;
        }
        let shown = match &status.snapshot.layout {
            Layout::Split { .. } => true,
            Layout::Single { session, .. } => session == selected,
            Layout::Empty => selected.is_empty(),
        };
        let snapshot = if shown {
            status.snapshot
        } else {
            let single = Operation::Single {
                session: selected.to_string(),
            };
            self.host
                .update_workspace(&single, None, Actor::Desktop)
                .unwrap_or_else(|_| {
                    self.reveal.store(true, Ordering::Release);
                    status.snapshot
                })
        };
        lock(&self.published).workspace = snapshot;
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
                self.select(id);
                continue;
            }
            if let Command::Inspect(tile) = command {
                // Validated against the workspace on the next publish.
                lock(&self.published).inspect_tile = tile;
                continue;
            }
            if let Command::DebugAt {
                target,
                panel,
                command,
            } = command
            {
                self.start_debug(target, panel, command);
                continue;
            }
            if let Command::CancelDebug(id) = command {
                self.cancel_debug(id);
                continue;
            }
            let trace_generation = matches!(command, Command::OpenTrace { .. })
                .then(|| self.result_generation.fetch_add(1, Ordering::AcqRel) + 1);
            self.pending.fetch_add(1, Ordering::AcqRel);
            let loops = self.clone();
            tokio::spawn(async move {
                let result = loops.execute(command, trace_generation).await;
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

    fn start_debug(&self, target: SessionTarget, panel: Panel, command: DebugCommand) {
        let id = self.debug_sequence.fetch_add(1, Ordering::Relaxed);
        let mut update = lock(&self.published);
        let mut active = lock(&self.active_debug);
        let allowed = active.len() < DEBUG_IN_FLIGHT
            && active
                .values()
                .filter(|(waiting, _)| waiting.id == target.id)
                .count()
                < DEBUG_PER_SESSION;
        let command = Arc::new(command);
        if panel == Panel::Result {
            self.result_generation.fetch_add(1, Ordering::AcqRel);
            if update
                .inspection
                .as_ref()
                .is_some_and(|item| item.target.is_none())
            {
                update.inspection = None;
            }
        }
        if update.debug_requests.len() == DEBUG_HISTORY {
            // The in-flight cap leaves space for completed entries to be evicted.
            let evict = update
                .debug_requests
                .iter()
                .position(|item| !matches!(item.status, DebugStatus::Running))
                .expect("bounded debugger queue has completed entries");
            update.debug_requests.remove(evict);
        }
        update.debug_requests.push_back(DebugRequest {
            id,
            target: target.clone(),
            panel,
            command: command.clone(),
            requires_control: debug_requires_control(&command),
            status: if allowed {
                DebugStatus::Running
            } else {
                DebugStatus::Failed {
                    kind: io::ErrorKind::WouldBlock,
                    message: "Debugger queue is full".into(),
                }
            },
        });
        if !allowed {
            return;
        }
        let cancellation = DebugCancellation::default();
        active.insert(id, (target.clone(), cancellation.clone()));
        drop(active);
        drop(update);
        let loops = self.clone();
        tokio::spawn(async move {
            let status = loops.run_debug(&target, command, cancellation).await;
            let mut update = lock(&loops.published);
            if let Some(request) = update
                .debug_requests
                .iter_mut()
                .find(|request| request.id == id && matches!(request.status, DebugStatus::Running))
            {
                request.status = status;
            }
            lock(&loops.active_debug).remove(&id);
        });
    }

    fn cancel_debug(&self, id: u64) {
        let mut update = lock(&self.published);
        if let Some(request) = update
            .debug_requests
            .iter_mut()
            .find(|request| request.id == id && matches!(request.status, DebugStatus::Running))
        {
            let mut active = lock(&self.active_debug);
            if let Some((_, cancellation)) = active.get(&id) {
                let dispatched = cancellation.cancel();
                if !dispatched {
                    active.remove(&id);
                }
                request.status = DebugStatus::Cancelled { dispatched };
            }
        }
    }

    async fn dispatch_debug(
        &self,
        target: &SessionTarget,
        command: &DebugCommand,
    ) -> io::Result<Value> {
        self.host
            .debug_at(
                &target.id,
                target.created_unix_ms,
                target.epoch,
                command,
                Actor::Desktop,
            )
            .await
    }

    fn target_exists(&self, target: &SessionTarget) -> bool {
        self.host
            .summary(&target.id)
            .is_ok_and(|summary| target.resolve(std::slice::from_ref(&summary)).is_some())
    }

    async fn run_debug(
        &self,
        target: &SessionTarget,
        command: Arc<DebugCommand>,
        cancellation: DebugCancellation,
    ) -> DebugStatus {
        if !self.target_exists(target) {
            return DebugStatus::Expired;
        }
        let reply = self
            .host
            .debug_at_cancellable(
                &target.id,
                (target.created_unix_ms, target.epoch),
                &command,
                Actor::Desktop,
                &cancellation,
            )
            .await;
        let status = match reply {
            Ok(value)
                if cancellation.check().is_ok()
                    && target.accepts_reply(&value)
                    && self.target_exists(target) =>
            {
                let loops = self.clone();
                let target = target.clone();
                tokio::task::spawn_blocking(move || {
                    loops.format_debug(&target, &command, value, &cancellation)
                })
                .await
                .map_err(io::Error::other)
                .and_then(|result| result)
            }
            Ok(_) => return DebugStatus::Expired,
            Err(error) => Err(error),
        };
        if !self.target_exists(target) {
            return DebugStatus::Expired;
        }
        match status {
            Ok((view, image)) => DebugStatus::Finished {
                view: Arc::new(view),
                image: image.map(Arc::new),
            },
            Err(error) => DebugStatus::Failed {
                kind: error.kind(),
                message: error.to_string(),
            },
        }
    }

    fn format_debug(
        &self,
        target: &SessionTarget,
        command: &DebugCommand,
        mut value: Value,
        cancellation: &DebugCancellation,
    ) -> io::Result<(Inspection, Option<DebugImage>)> {
        cancellation.check()?;
        if matches!(
            command,
            DebugCommand::GpuDraw { .. } | DebugCommand::ShaderTrace { .. }
        ) {
            if let Some(uri) = value["artifacts"][0]["uri"].as_str() {
                let bytes = self.host.read_debug_artifact(uri, 16 << 20, cancellation)?;
                cancellation.check()?;
                let capture: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                cancellation.check()?;
                if matches!(command, DebugCommand::GpuDraw { .. }) {
                    let mut previous = lock(&self.previous_draw);
                    if let Some((_, before)) = previous.as_ref().filter(|(id, _)| id == target) {
                        value["draw_changes"] =
                            serde_json::to_value(Inspection::draw_diff(before, &capture))
                                .map_err(io::Error::other)?;
                    }
                    *previous = Some((target.clone(), capture.clone()));
                }
                value["capture"] = capture;
            }
        }
        let mut image = None;
        if matches!(command, DebugCommand::Surface { .. }) {
            if let (Some(uri), Some(width), Some(height)) = (
                value["artifacts"][0]["uri"].as_str(),
                value["width"].as_u64(),
                value["height"].as_u64(),
            ) {
                if width > 1024 || height > 1024 {
                    return Err(io::Error::other("invalid surface image bounds"));
                }
                let size = (width * height * 4) as usize;
                let bytes = self.host.read_debug_artifact(uri, size, cancellation)?;
                if bytes.len() != size {
                    return Err(io::Error::other("invalid surface image bounds"));
                }
                image = Some(DebugImage {
                    target: target.clone(),
                    width: width as u32,
                    height: height as u32,
                    bgra: bytes,
                });
            }
        }
        cancellation.check()?;
        Ok((Inspection::from_value(&value, Panel::Result), image))
    }

    async fn execute(&self, command: Command, trace_generation: Option<u64>) -> io::Result<()> {
        let host = &self.host;
        let actor = Actor::Desktop;
        match command {
            Command::Create { kind, asset } => {
                let info = host.create(kind, asset.as_deref(), actor).await?;
                self.select(info.id.clone());
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
                self.select(session.clone());
                host.run(&session, actor).await?;
            }
            Command::Select(_) | Command::Inspect(_) => {}
            Command::DebugAt { .. } | Command::CancelDebug(_) => {
                unreachable!("dispatched by drain_commands")
            }
            Command::OpenTrace { path } => {
                let view = tokio::task::spawn_blocking(move || Inspection::load_trace(&path)).await;
                let mut update = lock(&self.published);
                if trace_generation == Some(self.result_generation.load(Ordering::Acquire))
                    && lock(&self.inspection_selection).panel == Panel::Result
                {
                    update.publish_inspection(InspectionSnapshot {
                        target: None,
                        panel: Panel::Result,
                        offset: None,
                        view: Arc::new(view.map_err(io::Error::other)??),
                    });
                }
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
            Command::Workspace(operation) => {
                let snapshot = host.update_workspace(&operation, None, actor)?;
                lock(&self.published).workspace = snapshot;
            }
            Command::WorkspaceAt(operation, revision) => {
                let snapshot = host.update_workspace(&operation, Some(revision), actor)?;
                lock(&self.published).workspace = snapshot;
            }
            Command::WorkspaceResolved(resolved) => host.report_workspace_resolved(resolved),
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
/// The host's controller service; one per desktop process, independent of selection.
struct Controllers {
    service: Arc<lotus_input::ControllerService>,
    routing: Arc<InputRouting>,
}

impl Controllers {
    /// Starts the device service and the `lotus-input-router` thread that applies its events
    /// until `shutdown`.
    fn start(routing: Arc<InputRouting>, shutdown: Arc<AtomicBool>) -> Option<Arc<Self>> {
        use lotus_input::gilrs_backend::GilrsBackend;
        let (service, events) = match lotus_input::ControllerService::start(GilrsBackend::new) {
            Ok(started) => started,
            Err(error) => {
                tracing::warn!(%error, "controller service not started");
                return None;
            }
        };
        let service = Arc::new(service);
        let thread_service = service.clone();
        let thread_routing = routing.clone();
        let spawned = std::thread::Builder::new()
            .name("lotus-input-router".into())
            .spawn(move || thread_routing.run(&thread_service, &events, &shutdown));
        if let Err(error) = spawned {
            tracing::warn!(%error, "controller routing not started");
            return None;
        }
        Some(Arc::new(Self { service, routing }))
    }

    /// Devices, service counters and routing for `frontend_metrics.controllers`.
    fn status(&self) -> Value {
        serde_json::json!({
            "devices": self.service.devices(),
            "diagnostics": self.service.diagnostics(),
            "routing": self.routing.status(),
        })
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lotus_session::{supervisor::SessionInfo, telemetry::Telemetry};

    fn summary() -> SessionSummary {
        let mut telemetry = Telemetry::new();
        telemetry.epoch = 4;
        SessionSummary {
            info: SessionInfo {
                id: "a".into(),
                kind: SessionKind::Secondary { parent: None },
                asset: None,
                created_unix_ms: 10,
            },
            telemetry,
        }
    }
    fn reply(target: &SessionTarget) -> InspectionSnapshot {
        InspectionSnapshot {
            target: Some(target.clone()),
            panel: Panel::Memory,
            offset: Some(0),
            view: Arc::new(Inspection::from_value(
                &serde_json::json!({"old_guest": 1}),
                Panel::Memory,
            )),
        }
    }
    fn surface_request(target: &SessionTarget) -> DebugRequest {
        let image = DebugImage {
            target: target.clone(),
            width: 1,
            height: 1,
            bgra: vec![0; 4],
        };
        DebugRequest {
            id: 1,
            target: target.clone(),
            panel: Panel::Gpu,
            requires_control: true,
            command: Arc::new(DebugCommand::Surface {
                address: 0,
                width: 1,
                height: 1,
                format: 0,
            }),
            status: DebugStatus::Finished {
                view: reply(target).view,
                image: Some(Arc::new(image)),
            },
        }
    }

    #[test]
    fn cached_and_late_inspection_cannot_cross_epoch_identity_or_target_changes() {
        for transition in 0..4 {
            let summary = summary();
            let target = SessionTarget::from_summary(&summary);
            let mut update = Update {
                sessions: vec![summary],
                inspected: "a".into(),
                ..Default::default()
            };
            assert!(
                update.publish_inspection(reply(&target)),
                "current reply accepted"
            );
            update.registers = Some((target.clone(), serde_json::json!({"r0":1})));
            update.debug_requests.push_back(surface_request(&target));
            match transition {
                0 => update.sessions[0].telemetry.epoch += 1,
                1 => update.sessions[0].info.created_unix_ms += 1,
                2 => update.inspected = "b".into(),
                _ => update.sessions.clear(),
            }
            update.retain_current_inspection();
            assert!(update.inspection.is_none());
            assert!(update.registers.is_none());
            assert!(update.debug_result(Panel::Gpu).is_none());
            // The request captured its target before the transition; it now
            // finishes after the publication lock has moved to the new state.
            assert!(
                !update.publish_inspection(reply(&target)),
                "late reply rejected"
            );
            assert!(update.inspection.is_none());
        }
    }

    #[test]
    fn local_trace_keeps_its_own_provenance_and_never_inherits_a_surface() {
        let summary = summary();
        let target = SessionTarget::from_summary(&summary);
        let mut update = Update {
            sessions: vec![summary],
            inspected: "a".into(),
            ..Default::default()
        };
        let trace = Inspection::from_value(&serde_json::json!({"trace":"local"}), Panel::Result);
        assert!(update.publish_inspection(InspectionSnapshot {
            target: None,
            panel: Panel::Result,
            offset: None,
            view: Arc::new(trace),
        }));
        update.debug_requests.push_back(surface_request(&target));
        update.sessions.clear();
        update.inspected.clear();
        update.retain_current_inspection();
        assert!(update.inspection.as_ref().unwrap().target.is_none());
        assert_eq!(update.inspection.as_ref().unwrap().view.rows.len(), 1);
        assert!(update.debug_result(Panel::Gpu).is_none());
    }
}
