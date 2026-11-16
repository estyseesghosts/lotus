//! Bridge between GPUI and the shared supervisor; no emulation on the UI thread.
//!
//! A background thread runs a Tokio runtime with four loops: UI commands (each
//! its own task, so one slow worker never stalls the others), the shared pump
//! (gauges and continuous running), coalesced live input, and the selected
//! session's preview. With [`McpMode::Stdio`] the same runtime also serves the
//! MCP tools over stdio against the same supervisor, so an agent and the person
//! at the window drive one set of sessions.
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::inspection::{Inspection, Panel};
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
    pub mcp: McpLink,
    /// Commands still in flight.
    pub pending: usize,
}

pub struct DesktopController {
    pub commands: SyncSender<Command>,
    pub latest: Arc<Mutex<Update>>,
    pub input: Arc<Mutex<BTreeMap<String, InputState>>>,
    /// Fetch registers while paused only when the inspector is open.
    pub inspector: Arc<AtomicBool>,
    pub inspection_selection: Arc<Mutex<(Panel, u32)>>,
    host: Arc<Supervisor>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

const TICK: Duration = Duration::from_millis(33);
const PUMP_PERIOD: Duration = Duration::from_millis(100);
const INPUT_PERIOD: Duration = Duration::from_millis(8);
const CATALOG_PERIOD: Duration = Duration::from_secs(1);

impl DesktopController {
    pub fn start(config: HostConfig, mcp: McpMode) -> io::Result<Self> {
        let host = Arc::new(Supervisor::new(config)?);
        let (sender, receiver) = mpsc::sync_channel::<Command>(32);
        let latest = Arc::new(Mutex::new(Update {
            max_sessions: host.max_sessions(),
            control: host.allows_control(),
            ..Update::default()
        }));
        let input: Arc<Mutex<BTreeMap<String, InputState>>> = Arc::default();
        let inspector = Arc::new(AtomicBool::new(false));
        let inspection_selection = Arc::new(Mutex::new((Panel::Cpu, 0)));
        let shutdown = Arc::new(AtomicBool::new(false));
        let context = Loops {
            host: host.clone(),
            published: latest.clone(),
            input: input.clone(),
            inspector: inspector.clone(),
            inspection_selection: inspection_selection.clone(),
            previous_draw: Arc::default(),
            shutdown: shutdown.clone(),
            pending: Arc::new(AtomicUsize::new(0)),
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
            input,
            inspector,
            inspection_selection,
            host,
            shutdown,
            thread: Some(thread),
        })
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
    published: Arc<Mutex<Update>>,
    input: Arc<Mutex<BTreeMap<String, InputState>>>,
    inspector: Arc<AtomicBool>,
    inspection_selection: Arc<Mutex<(Panel, u32)>>,
    previous_draw: Arc<Mutex<Option<(String, Value)>>>,
    shutdown: Arc<AtomicBool>,
    pending: Arc<AtomicUsize>,
}

impl Loops {
    async fn run(self, receiver: mpsc::Receiver<Command>, mcp: McpMode) {
        let mut background = Vec::new();
        if mcp == McpMode::Stdio {
            background.push(tokio::spawn(self.clone().serve_mcp()));
        }
        background.push(tokio::spawn(self.clone().pump_loop()));
        background.push(tokio::spawn(self.clone().input_loop()));
        let mut sequence = 0u64;
        let mut audio_selected = String::new();
        let mut previewed_session = String::new();
        let mut registers_key = None;
        let mut inspection_key = None;
        let mut inspected_at = Instant::now();
        let mut catalog_at: Option<Instant> = None;
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
            let selected = {
                let mut update = lock(&self.published);
                if !sessions.iter().any(|s| s.info.id == update.selected) {
                    update.selected = sessions
                        .first()
                        .map(|s| s.info.id.clone())
                        .unwrap_or_default();
                }
                update.sessions = sessions.clone();
                update.pending = self.pending.load(Ordering::Acquire);
                update.selected.clone()
            };
            if !selected.is_empty() {
                if audio_selected != selected
                    && self.host.allows_control()
                    && self.host.focus_audio(&selected, true).await.is_ok()
                {
                    audio_selected = selected.clone();
                }
                if previewed_session != selected {
                    // The window drops its images on a session switch (lotus_window.rs).
                    self.host.forget_preview(&selected);
                    previewed_session = selected.clone();
                }
                if let Some(frames) = self.host.preview(&selected).await {
                    sequence += 1;
                    lock(&self.published).preview = Some(Preview {
                        session: selected.clone(),
                        sequence,
                        frames,
                    });
                }
                let paused = sessions
                    .iter()
                    .find(|s| s.info.id == selected)
                    .filter(|s| s.telemetry.state == SessionState::Paused && !s.telemetry.playing);
                let key = paused.map(|s| (selected.clone(), s.telemetry.epoch, s.telemetry.frames));
                if self.inspector.load(Ordering::Acquire) && key.is_some() && key != registers_key {
                    if let Ok(value) = self.host.registers(&selected, 0).await {
                        lock(&self.published).registers = Some((selected.clone(), value));
                        registers_key = key;
                    }
                } else if key.is_none() {
                    registers_key = None;
                }
                let (panel, offset) = *lock(&self.inspection_selection);
                let view_key = (selected.clone(), panel, offset);
                if self.inspector.load(Ordering::Acquire)
                    && (inspection_key.as_ref() != Some(&view_key)
                        || inspected_at.elapsed() >= Duration::from_millis(500))
                {
                    if let Some(command) = panel.command(offset) {
                        let value = match self.host.debug(&selected, &command, Actor::Desktop).await
                        {
                            Ok(value) => value,
                            Err(error) => serde_json::json!({"error":error.to_string()}),
                        };
                        let mut value = value;
                        if panel == Panel::Cpu {
                            if let Some(pc) = value["registers"][15].as_u64() {
                                let command = DebugCommand::Disassemble {
                                    address: pc as u32,
                                    length: 64,
                                    thumb: value["cpsr"].as_u64().unwrap_or(0) & 32 != 0,
                                };
                                if let Ok(assembly) =
                                    self.host.debug(&selected, &command, Actor::Desktop).await
                                {
                                    value["disassembly"] = assembly;
                                }
                            }
                        }
                        lock(&self.published).inspection = Some((
                            selected.clone(),
                            panel,
                            Arc::new(Inspection::from_value(&value, panel)),
                        ));
                    }
                    inspection_key = Some(view_key);
                    inspected_at = Instant::now();
                }
            }
            tokio::time::sleep(TICK).await;
        }
        for task in background {
            task.abort();
        }
        self.host.close().await;
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

    async fn pump_loop(self) {
        loop {
            self.host.pump().await;
            tokio::time::sleep(PUMP_PERIOD).await;
        }
    }

    /// Complete input states coalesce here, independent of the command bound.
    async fn input_loop(self) {
        loop {
            let states = std::mem::take(&mut *lock(&self.input));
            for (id, state) in states {
                let _ = self.host.input(&id, state, "live", Actor::Desktop).await;
            }
            tokio::time::sleep(INPUT_PERIOD).await;
        }
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
