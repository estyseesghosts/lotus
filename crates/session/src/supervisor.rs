//! Local host policy: approved assets, four worker slots, bounded artifacts.
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use lotus_protocol::envelope::SessionUuid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::native_session::NativeSession;

pub const MAX_WORKERS: usize = 4;
pub const MAX_ARTIFACTS: usize = 128;

/// Host configuration, never accepted as a tool-call argument.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    pub worker: PathBuf,
    pub storage_root: PathBuf,
    pub assets: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub allow_control: bool,
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

/// No shell execution, arbitrary caller paths or remote listener.
pub struct Supervisor {
    config: HostConfig,
    sessions: BTreeMap<String, NativeSession>,
    artifacts: BTreeMap<String, (Artifact, PathBuf)>,
}

impl Supervisor {
    pub fn new(mut config: HostConfig) -> io::Result<Self> {
        config.worker = config.worker.canonicalize()?;
        for path in config.assets.values_mut() {
            *path = path.canonicalize()?;
        }
        fs::create_dir_all(&config.storage_root)?;
        config.storage_root = config.storage_root.canonicalize()?;
        Ok(Self {
            config,
            sessions: BTreeMap::new(),
            artifacts: BTreeMap::new(),
        })
    }

    #[must_use]
    pub fn sessions(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }

    #[must_use]
    pub fn assets(&self) -> Vec<String> {
        self.config.assets.keys().cloned().collect()
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

    pub async fn create(&mut self, asset: &str) -> io::Result<String> {
        self.require_control()?;
        if self.sessions.len() >= MAX_WORKERS {
            return Err(io::Error::other("four-worker resource limit"));
        }
        let content = self.config.assets.get(asset).ok_or_else(|| {
            io::Error::new(io::ErrorKind::PermissionDenied, "unknown asset handle")
        })?;
        let id = SessionUuid::generate().to_string();
        let root = self.config.storage_root.join(&id);
        let mut session = NativeSession::spawn(&self.config.worker, &root).await?;
        session.load(content).await?;
        self.sessions.insert(id.clone(), session);
        Ok(id)
    }

    pub fn session(&mut self, id: &str) -> io::Result<&mut NativeSession> {
        self.sessions.get_mut(id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "session is not owned by this host",
            )
        })
    }

    pub async fn stop(&mut self, id: &str) -> io::Result<()> {
        self.require_control()?;
        let mut session = self
            .sessions
            .remove(id)
            .ok_or_else(|| io::Error::other("unknown session"))?;
        session.stop().await
    }

    pub async fn capture(&mut self, id: &str) -> io::Result<Vec<Artifact>> {
        if self.artifacts.len() + 2 > MAX_ARTIFACTS {
            return Err(io::Error::other(
                "artifact quota reached; restart host to reset catalog",
            ));
        }
        let frames = self.session(id)?.capture().await?;
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
            self.artifacts
                .insert(artifact.uri.clone(), (artifact.clone(), path));
            artifacts.push(artifact);
        }
        Ok(artifacts)
    }

    #[must_use]
    pub fn artifacts(&self) -> Vec<Artifact> {
        self.artifacts
            .values()
            .map(|(artifact, _)| artifact.clone())
            .collect()
    }

    /// Resolve only catalogued immutable captures, never interpret a URI as a path.
    pub fn read_artifact(&self, uri: &str) -> io::Result<Vec<u8>> {
        let (artifact, path) = self.artifacts.get(uri).ok_or_else(|| {
            io::Error::new(io::ErrorKind::PermissionDenied, "unknown artifact resource")
        })?;
        let data = fs::read(path)?;
        if data.len() != artifact.bytes || format!("{:x}", Sha256::digest(&data)) != artifact.sha256
        {
            return Err(io::Error::other("artifact identity mismatch"));
        }
        Ok(data)
    }
}
