//! Native worker process handling.
//!
//! The worker is a separate OS process running a modified Azahar core. This
//! module owns starting it, handing it its private storage and stopping it
//! without leaving orphans behind.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use thiserror::Error;

/// Azahar branch flavour a worker was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerFlavor {
    /// `integration` branch: upstream minimal worker hooks.
    Integration,
    /// `experimental` branch: tracing, debugger and accuracy work.
    Experimental,
}

impl WorkerFlavor {
    /// Branch name on the Azahar fork.
    pub fn branch(&self) -> &'static str {
        match self {
            WorkerFlavor::Integration => "integration",
            WorkerFlavor::Experimental => "experimental",
        }
    }
}

/// Errors raised while launching or supervising a worker.
#[derive(Debug, Error)]
pub enum WorkerLaunchError {
    /// The worker executable was not found.
    #[error("worker executable not found: {0}")]
    NotFound(PathBuf),
    /// The worker could not be started.
    #[error("failed to launch worker: {0}")]
    Launch(#[source] std::io::Error),
}

/// How a worker process should be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSpec {
    /// Path of the worker executable.
    pub executable: PathBuf,
    /// Private storage root handed to the worker.
    pub storage_root: PathBuf,
    /// Additional command line arguments.
    pub args: Vec<String>,
    /// Environment variables added to the worker environment.
    pub env: Vec<(String, String)>,
}

impl WorkerSpec {
    /// A worker that runs against `storage_root`.
    pub fn new(executable: impl Into<PathBuf>, storage_root: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            storage_root: storage_root.into(),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    /// Add a command line argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Add an environment variable.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// A running worker process.
#[derive(Debug)]
pub struct WorkerProcess {
    child: Child,
    executable: PathBuf,
}

impl WorkerProcess {
    /// Start a worker.
    pub fn spawn(spec: &WorkerSpec) -> Result<Self, WorkerLaunchError> {
        if !spec.executable.is_file() {
            return Err(WorkerLaunchError::NotFound(spec.executable.clone()));
        }

        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.args)
            .envs(spec.env.iter().cloned())
            .env("LOTUS_STORAGE_ROOT", &spec.storage_root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // Do not let the worker pop a console window on Windows.
            command.creation_flags(0x0800_0000);
        }

        let child = command.spawn().map_err(WorkerLaunchError::Launch)?;
        Ok(Self {
            child,
            executable: spec.executable.clone(),
        })
    }

    /// Process id of the worker.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Path of the worker executable.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Whether the worker is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Ask the worker to stop and wait for it, escalating if it ignores the
    /// request.
    pub fn terminate(&mut self) {
        if self.is_running() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_executable_is_reported() {
        let spec = WorkerSpec::new(
            std::env::temp_dir().join("lotus-no-such-worker"),
            std::env::temp_dir().join("lotus-no-such-storage"),
        );
        assert!(matches!(
            WorkerProcess::spawn(&spec),
            Err(WorkerLaunchError::NotFound(_))
        ));
    }

    #[test]
    fn flavor_maps_to_branch_names() {
        assert_eq!(WorkerFlavor::Integration.branch(), "integration");
        assert_eq!(WorkerFlavor::Experimental.branch(), "experimental");
    }
}
