//! Worker process lifecycle and session isolation.
//!
//! One emulator instance is one OS process running the native Azahar worker.
//! The session manager owns those processes, their private storage roots and
//! their state. It never links against `Core::System` and it never lets a
//! worker failure take down the application or a sibling instance.

pub mod storage;
pub mod worker;

pub use storage::StorageLayout;
pub use worker::{WorkerFlavor, WorkerLaunchError, WorkerProcess, WorkerSpec};

use lotus_protocol::capabilities::ConsoleModel;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Identifier of a single emulator instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(pub u128);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "session-{:032x}", self.0)
    }
}

/// Lifecycle state of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Created, worker not started yet.
    Created,
    /// Worker running, content not loaded.
    Idle,
    /// Content loaded, emulation running.
    Running,
    /// Content loaded, emulation paused at a safe point.
    Paused,
    /// Worker exited normally.
    Stopped,
    /// Worker exited unexpectedly.
    Crashed,
}

impl SessionState {
    /// Whether the session can still issue emulation commands.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            SessionState::Idle | SessionState::Running | SessionState::Paused
        )
    }
}

/// Configuration of a session.
///
/// Every field that affects execution must be recorded in a run manifest so
/// that a result can be reproduced from a known configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Emulated console model.
    pub model: ConsoleModel,
    /// Render backend requested from the worker.
    pub renderer: String,
    /// Azahar branch flavour the worker was built from.
    pub worker_flavor: WorkerFlavor,
    /// Initial virtual tick value for deterministic runs.
    pub deterministic_initial_ticks: Option<u64>,
    /// Where the session keeps its private storage.
    pub storage_root: PathBuf,
    /// Free-form run parameters.
    pub parameters: BTreeMap<String, String>,
}

impl SessionConfig {
    /// Create a configuration with defaults for everything but the storage root.
    pub fn new(storage_root: impl Into<PathBuf>) -> Self {
        Self {
            model: ConsoleModel::Old3ds,
            renderer: "software".to_string(),
            worker_flavor: WorkerFlavor::Integration,
            deterministic_initial_ticks: None,
            storage_root: storage_root.into(),
            parameters: BTreeMap::new(),
        }
    }
}

/// A live session: its worker process, its state and its storage.
#[derive(Debug)]
pub struct Session {
    /// Session identifier.
    pub id: SessionId,
    /// Configuration the session was created with.
    pub config: SessionConfig,
    /// Private storage layout for this session.
    pub storage: StorageLayout,
    /// Worker process, absent before the worker is started.
    pub worker: Option<WorkerProcess>,
    /// Current lifecycle state.
    pub state: SessionState,
}

impl Session {
    /// Create a session with a fresh, isolated storage layout.
    pub fn new(id: SessionId, config: SessionConfig) -> std::io::Result<Self> {
        let storage = StorageLayout::create(&config.storage_root, id)?;
        Ok(Self {
            id,
            config,
            storage,
            worker: None,
            state: SessionState::Created,
        })
    }

    /// Whether the session is still usable.
    pub fn is_active(&self) -> bool {
        self.state.is_active()
    }
}

/// Registry of sessions owned by this application.
#[derive(Debug, Default)]
pub struct SessionManager {
    sessions: BTreeMap<SessionId, Session>,
}

impl SessionManager {
    /// Create an empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create and register a session.
    pub fn create(&mut self, config: SessionConfig) -> std::io::Result<SessionId> {
        let id = SessionId(next_session_id());
        let session = Session::new(id, config)?;
        self.sessions.insert(id, session);
        Ok(id)
    }

    /// Look up a session.
    pub fn get(&self, id: &SessionId) -> Option<&Session> {
        self.sessions.get(id)
    }

    /// Look up a session for mutation.
    pub fn get_mut(&mut self, id: &SessionId) -> Option<&mut Session> {
        self.sessions.get_mut(id)
    }

    /// Identifiers of all registered sessions.
    pub fn ids(&self) -> Vec<SessionId> {
        self.sessions.keys().copied().collect()
    }

    /// Number of registered sessions.
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether no session is registered.
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Remove a session and stop its worker.
    pub fn shutdown(&mut self, id: &SessionId) -> Option<Session> {
        let mut session = self.sessions.remove(id)?;
        if let Some(mut worker) = session.worker.take() {
            worker.terminate();
        }
        session.state = SessionState::Stopped;
        Some(session)
    }
}

/// Monotonic session counter mixed with the process id.
fn next_session_id() -> u128 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id() as u128;
    (pid << 64) | counter as u128
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lotus-session-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn sessions_get_isolated_storage() {
        let root = temp_root("isolated");
        let mut manager = SessionManager::new();
        let a = manager.create(SessionConfig::new(root.join("a"))).unwrap();
        let b = manager.create(SessionConfig::new(root.join("b"))).unwrap();

        let storage_a = manager.get(&a).unwrap().storage.root();
        let storage_b = manager.get(&b).unwrap().storage.root();
        assert_ne!(storage_a, storage_b);
        assert!(storage_a.is_dir());
        assert!(storage_b.is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shutdown_stops_the_session() {
        let root = temp_root("shutdown");
        let mut manager = SessionManager::new();
        let id = manager.create(SessionConfig::new(root.join("s"))).unwrap();
        assert_eq!(manager.len(), 1);
        let session = manager.shutdown(&id).unwrap();
        assert_eq!(session.state, SessionState::Stopped);
        assert!(manager.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_ids_are_unique() {
        let root = temp_root("ids");
        let mut manager = SessionManager::new();
        let a = manager.create(SessionConfig::new(root.join("a"))).unwrap();
        let b = manager.create(SessionConfig::new(root.join("b"))).unwrap();
        assert_ne!(a, b);
        let _ = std::fs::remove_dir_all(&root);
    }
}
pub mod controlled_worker;
