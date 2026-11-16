//! Bounded bridge between GPUI and the native supervisor; no emulation on UI thread.
use std::collections::BTreeMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use lotus_session::native_session::{Frame, InputState};
use lotus_session::supervisor::{HostConfig, Supervisor};
use lotus_session::SessionState;

#[derive(Clone)]
pub enum Command {
    Create(String),
    Select(String),
    Play(String),
    Pause(String),
    Reset(String),
    Stop(String),
    Abort(String),
}
#[derive(Default)]
pub struct SessionView {
    pub id: String,
    pub state: String,
    pub frame: u64,
    pub images: Option<Vec<Frame>>,
    pub playing: bool,
    pub error: String,
}
#[derive(Default)]
pub struct Update {
    pub sessions: BTreeMap<String, SessionView>,
    pub error: String,
    pub selected: String,
}

/// Snapshots replace older frames; complete input states coalesce independently of commands.
pub struct DesktopController {
    pub commands: SyncSender<Command>,
    pub latest: Arc<Mutex<Update>>,
    pub input: Arc<Mutex<BTreeMap<String, InputState>>>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DesktopController {
    pub fn start(config: HostConfig) -> io::Result<Self> {
        let mut host = Supervisor::new(config)?;
        let (sender, receiver) = mpsc::sync_channel(32);
        let latest = Arc::new(Mutex::new(Update::default()));
        let input: Arc<Mutex<BTreeMap<String, InputState>>> = Arc::default();
        let published = latest.clone();
        let pending_input = input.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let requested_shutdown = shutdown.clone();
        let thread = std::thread::Builder::new()
            .name("lotus-supervisor".into())
            .spawn(move || {
                let runtime = tokio::runtime::Runtime::new().expect("supervisor runtime");
                runtime.block_on(async move {
                    let mut selected = String::new();
                    let mut playing: BTreeMap<String, bool> = BTreeMap::new();
                    loop {
                        if requested_shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        let mut connected = true;
                        for _ in 0..32 {
                            let command = match receiver.try_recv() {
                                Ok(command) => command,
                                Err(TryRecvError::Empty) => break,
                                Err(TryRecvError::Disconnected) => {
                                    connected = false;
                                    break;
                                }
                            };
                            let result: io::Result<()> = async {
                                match command {
                                    Command::Create(asset) => {
                                        let id = host.create(&asset).await?;
                                        playing.insert(id.clone(), false);
                                        if let Ok(mut update) = published.lock() {
                                            update.sessions.insert(
                                                id.clone(),
                                                SessionView {
                                                    id: id.clone(),
                                                    state: "paused".into(),
                                                    ..SessionView::default()
                                                },
                                            );
                                        }
                                        selected = id;
                                        if let Ok(mut update) = published.lock() {
                                            update.selected = selected.clone();
                                        }
                                    }
                                    Command::Select(id) => {
                                        selected = id;
                                        if let Ok(mut update) = published.lock() {
                                            update.selected = selected.clone();
                                        }
                                    }
                                    Command::Play(id) => {
                                        host.require_control()?;
                                        host.session(&id)?.run().await?;
                                        playing.insert(id, true);
                                    }
                                    Command::Pause(id) => {
                                        host.require_control()?;
                                        playing.insert(id.clone(), false);
                                        host.session(&id)?.pause().await?;
                                    }
                                    Command::Reset(id) => {
                                        host.require_control()?;
                                        playing.insert(id.clone(), false);
                                        host.session(&id)?.pause().await?;
                                        host.session(&id)?.reset().await?;
                                    }
                                    Command::Stop(id) => {
                                        playing.remove(&id);
                                        let result = host.stop(&id).await;
                                        if let Ok(mut update) = published.lock() {
                                            update.sessions.remove(&id);
                                        }
                                        result?;
                                    }
                                    Command::Abort(id) => {
                                        host.require_control()?;
                                        host.session(&id)?.abort().await;
                                        playing.insert(id, false);
                                    }
                                }
                                Ok(())
                            }
                            .await;
                            if let Err(error) = result {
                                if let Ok(mut update) = published.lock() {
                                    update.error = error.to_string();
                                }
                            }
                        }
                        if !connected {
                            break;
                        }
                        let inputs = pending_input
                            .lock()
                            .map(|mut states| std::mem::take(&mut *states))
                            .unwrap_or_default();
                        for (id, state) in inputs {
                            if let Ok(session) = host.session(&id) {
                                let _ = session.input(state, "live").await;
                            }
                        }
                        for id in host.sessions() {
                            let result = async {
                                let session = host.session(&id)?;
                                if session.state() == SessionState::Crashed {
                                    return Err(io::Error::other(
                                        "Worker crashed; stop it to release this slot.",
                                    ));
                                }
                                let status = session.status().await?;
                                let frame = status["frames"].as_u64().unwrap_or(0);
                                if playing.get(&id) == Some(&true)
                                    && session.state() == SessionState::Paused
                                {
                                    session.run().await?;
                                }
                                let images = if id == selected && frame > 0 {
                                    Some(session.capture().await?)
                                } else {
                                    None
                                };
                                Ok((
                                    frame,
                                    format!("{:?}", session.state()).to_lowercase(),
                                    images,
                                ))
                            }
                            .await;
                            if let Ok(mut update) = published.lock() {
                                if let Some(view) = update.sessions.get_mut(&id) {
                                    match result {
                                        Ok((frame, state, images)) => {
                                            view.frame = frame;
                                            view.state = state;
                                            if images.is_some() {
                                                view.images = images;
                                            }
                                            view.error.clear();
                                        }
                                        Err(error) => {
                                            view.error = error.to_string();
                                            view.state = "crashed".into();
                                            view.images = None;
                                            playing.insert(id.clone(), false);
                                        }
                                    }
                                    view.playing = playing.get(&id) == Some(&true);
                                }
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(33)).await;
                    }
                    for id in host.sessions() {
                        let _ = tokio::time::timeout(Duration::from_secs(3), host.stop(&id)).await;
                    }
                });
            })?;
        Ok(Self {
            commands: sender,
            latest,
            input,
            shutdown,
            thread: Some(thread),
        })
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
