//! One listener per session, shared by every adapter of the host.
use crate::{Server, Target};
use lotus_session::supervisor::Supervisor;
use std::{
    collections::BTreeMap,
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

type Slot = tokio::sync::Mutex<Option<(Target, Server)>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Stopped,
    Changing,
    Listening,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Changing => "changing",
            Self::Listening => "listening",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    pub address: Option<SocketAddr>,
    /// A connected RSP client holds the target's exclusive mutation lease.
    pub leased: bool,
    pub backend: String,
    pub error: Option<String>,
}

impl Status {
    /// Known worker branches are identifiable from their handshake. Workers
    /// without a recognized branch are checked by the native probe on Start.
    pub fn backend_support(&self) -> Option<bool> {
        match self.backend.as_str() {
            "CITnLO" => Some(true),
            "AZAHAR" => Some(false),
            _ => None,
        }
    }
}

/// A host service, independent of any individual MCP client or desktop window.
/// The registry lock only covers slot lookup. Worker IPC and listener shutdown
/// serialize per session so a stalled target cannot delay another.
pub struct Manager {
    host: Arc<Supervisor>,
    registry: Mutex<Registry>,
}

#[derive(Default)]
struct Registry {
    closed: bool,
    slots: BTreeMap<String, Arc<Slot>>,
}

impl Manager {
    pub fn new(host: Arc<Supervisor>) -> Self {
        Self {
            host,
            registry: Mutex::default(),
        }
    }

    pub fn status(&self, target: &Target) -> io::Result<Status> {
        target.check(&self.host)?;
        let mut status = Status {
            state: State::Stopped,
            address: None,
            leased: false,
            backend: self.host.summary(&target.session)?.telemetry.backend_name,
            error: None,
        };
        if let Some(slot) = self.slot(&target.session) {
            match slot.try_lock() {
                Ok(entry) => {
                    if let Some((bound, server)) = &*entry {
                        if bound == target {
                            if server.finished() {
                                status.error = server.failure();
                            } else {
                                status.state = State::Listening;
                                status.address = Some(server.address);
                                status.leased = server.leased();
                            }
                        } else if !server.finished() {
                            status.state = State::Changing;
                        }
                    }
                }
                Err(_) => status.state = State::Changing,
            }
        }
        target.check(&self.host)?;
        Ok(status)
    }

    pub async fn start(&self, target: Target, port: u16) -> io::Result<Status> {
        self.host.require_control()?;
        target.check(&self.host)?;
        let mut entry = {
            let mut registry = self.registry.lock().unwrap_or_else(|err| err.into_inner());
            if registry.closed {
                return Err(crate::disconnected());
            }
            registry
                .slots
                .retain(|id, slot| Arc::strong_count(slot) > 1 || self.host.summary(id).is_ok());
            registry
                .slots
                .entry(target.session.clone())
                .or_default()
                .clone()
                .try_lock_owned()
                .map_err(|_| changing())?
        };
        let host = self.host.clone();
        let bound = target.clone();
        // Once assigned, let the operation drain even if the originating MCP
        // request is cancelled or its transport disconnects during the probe.
        tokio::spawn(async move {
            if let Some((previous, server)) = &*entry {
                if previous == &bound && !server.finished() {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("GDB server already started at {}", server.address),
                    ));
                }
                if !server.finished() {
                    server.stop().await?;
                }
            }
            *entry = None;
            let server = Server::start_at(host, bound.clone(), port).await?;
            *entry = Some((bound, server));
            Ok(())
        })
        .await
        .map_err(io::Error::other)??;
        self.status(&target)
    }

    pub async fn stop(&self, target: &Target) -> io::Result<()> {
        self.host.require_control()?;
        target.check(&self.host)?;
        if let Some(slot) = self.slot(&target.session) {
            let mut entry = slot.try_lock_owned().map_err(|_| changing())?;
            if entry.as_ref().is_some_and(|(bound, _)| bound == target) {
                tokio::spawn(async move { stop(&mut entry).await })
                    .await
                    .map_err(io::Error::other)??;
            }
        }
        target.check(&self.host)
    }

    /// Drain all active listeners before a host-owned runtime exits.
    pub async fn close(&self) {
        let slots = {
            let mut registry = self.registry.lock().unwrap_or_else(|err| err.into_inner());
            registry.closed = true;
            std::mem::take(&mut registry.slots)
        };
        let mut tasks = tokio::task::JoinSet::new();
        for (id, slot) in slots {
            tasks.spawn(async move {
                let mut entry = slot.lock().await;
                (id, stop(&mut entry).await)
            });
        }
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok((id, Err(error))) => eprintln!("GDB stop on {id}: {error}"),
                Err(error) => eprintln!("GDB listener cleanup: {error}"),
                Ok((_, Ok(()))) => {}
            }
        }
    }

    fn slot(&self, session: &str) -> Option<Arc<Slot>> {
        self.registry
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .slots
            .get(session)
            .cloned()
    }
}

fn changing() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "GDB listener is starting or stopping",
    )
}

async fn stop(entry: &mut Option<(Target, Server)>) -> io::Result<()> {
    if let Some((_, server)) = entry {
        let result = server.stop().await;
        *entry = None;
        result?;
    }
    Ok(())
}
