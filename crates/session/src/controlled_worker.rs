//! Supervisor for a single inherited private pipe connection.
//!
//! All exchanges are serialized by mutable ownership. A failed or timed-out
//! exchange destroys the connection: a possibly committed request is never retried.
use std::fs::{self, File};
use std::io;

use std::process::Stdio;
use std::time::Duration;

use lotus_protocol::control::{self, Method, Reply, Request, Status};
use lotus_protocol::envelope::SessionUuid;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::{SessionState, WorkerSpec};

/// Owns the child and its unshared pipe ends. Drop kills the direct child.
#[derive(Debug)]
pub struct ControlledWorker {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    session: String,
    next_request: u64,
    epoch: u64,
    state: SessionState,
    timeout: Duration,
    root_lock: Option<File>,
    #[cfg(windows)]
    job: Option<crate::process_job::ProcessJob>,
}

impl ControlledWorker {
    /// Launch, retain stderr locally, and complete a bounded handshake.
    pub async fn spawn(spec: &WorkerSpec, timeout: Duration) -> io::Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "timeout must be in (0, 30s]",
            ));
        }
        fs::create_dir_all(&spec.storage_root)?;
        let root_lock = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(spec.storage_root.join("worker.lock"))?;
        root_lock.try_lock().map_err(io::Error::other)?;
        let stderr = File::create(spec.storage_root.join("worker.stderr.log"))?;
        let mut command = Command::new(&spec.executable);
        command
            .args(&spec.args)
            .envs(spec.env.iter().cloned())
            .env("LOTUS_STORAGE_ROOT", &spec.storage_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0004);
        let mut child = command.spawn()?;
        #[cfg(windows)]
        let job = match crate::process_job::ProcessJob::attach_and_resume(
            child.id().expect("new child has PID"),
        ) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill().await;
                return Err(error);
            }
        };
        let input = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing child stdin"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing child stdout"))?;
        let mut worker = Self {
            child,
            input,
            output,
            session: SessionUuid::generate().to_string(),
            next_request: 1,
            epoch: 1,
            state: SessionState::Created,
            timeout,
            root_lock: Some(root_lock),
            #[cfg(windows)]
            job: Some(job),
        };
        worker.request(Method::Hello).await?;
        worker.state = SessionState::Idle;
        Ok(worker)
    }

    /// Current supervisor state; poll status to detect a newly exited process.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.state
    }

    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Child OS identifier, absent after reaping.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Child working set in bytes, absent after reaping or on unsupported hosts.
    #[must_use]
    pub fn working_set(&self) -> Option<u64> {
        crate::process_memory::working_set(&self.child)
    }

    /// Query the worker without any GUI dependencies.
    pub async fn status(&mut self) -> io::Result<Reply> {
        self.request(Method::Status).await
    }

    /// Complete shutdown and reap; kill on any failed exchange or exit deadline.
    pub async fn stop(&mut self) -> io::Result<()> {
        self.request(Method::Stop).await?;
        match tokio::time::timeout(self.timeout, self.child.wait()).await {
            Ok(Ok(status)) if status.success() => {
                self.state = SessionState::Stopped;
                self.release_resources();
                Ok(())
            }
            _ => {
                self.fail().await;
                Err(io::Error::other("worker failed to exit cleanly"))
            }
        }
    }

    async fn fail(&mut self) {
        self.state = SessionState::Crashed;
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        self.release_resources();
    }

    /// Terminate this worker and its contained descendants; retain logs.
    pub async fn abort(&mut self) {
        self.fail().await;
    }

    fn release_resources(&mut self) {
        #[cfg(windows)]
        {
            self.job.take();
        }
        self.root_lock.take();
    }

    pub async fn command(
        &mut self,
        method: Method,
        params: serde_json::Value,
    ) -> io::Result<Reply> {
        if matches!(self.state, SessionState::Stopped | SessionState::Crashed) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "worker is no longer active",
            ));
        }
        let request = Request {
            version_major: lotus_protocol::PROTOCOL_VERSION_MAJOR,
            version_minor: lotus_protocol::PROTOCOL_VERSION_MINOR,
            request_id: self.next_request,
            session: self.session.clone(),
            epoch: self.epoch,
            method,
            params,
        };
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or_else(|| io::Error::other("request ID exhausted"))?;
        let result = tokio::time::timeout(self.timeout, self.exchange(&request)).await;
        let reply = match result {
            Ok(Ok(reply)) => reply,
            Ok(Err(error)) => {
                self.fail().await;
                return Err(error);
            }
            Err(_) => {
                self.fail().await;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "worker exchange deadline exceeded; result unknown",
                ));
            }
        };
        if !lotus_protocol::is_compatible(reply.version_major, reply.version_minor)
            || reply.request_id != request.request_id
            || reply.session != self.session
            || reply.epoch
                != self.epoch
                    + u64::from(
                        matches!(method, Method::Load | Method::Reset)
                            && reply.status == Status::Ok,
                    )
            || !matches!(
                reply.state.as_str(),
                "idle" | "running" | "paused" | "stopped"
            )
        {
            self.fail().await;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid worker reply identity, epoch, status or state",
            ));
        }
        self.epoch = reply.epoch;
        self.state = match reply.state.as_str() {
            "running" => SessionState::Running,
            "paused" => SessionState::Paused,
            "stopped" => SessionState::Stopped,
            _ => SessionState::Idle,
        };
        Ok(reply)
    }

    async fn request(&mut self, method: Method) -> io::Result<Reply> {
        let reply = self.command(method, serde_json::Value::Null).await?;
        if reply.status != Status::Ok {
            return Err(io::Error::other(format!(
                "worker rejected {method:?}: {:?}",
                reply.status
            )));
        }
        Ok(reply)
    }

    async fn exchange(&mut self, request: &Request) -> io::Result<Reply> {
        self.input.write_all(&control::encode(request)?).await?;
        self.input.flush().await?;
        let length = self.output.read_u32_le().await?;
        control::validate_length(length)?;
        let mut payload = vec![0; length as usize];
        self.output.read_exact(&mut payload).await?;
        serde_json::from_slice(&payload).map_err(io::Error::other)
    }
}

impl Drop for ControlledWorker {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        self.release_resources();
    }
}
