//! Supervisor for a single inherited private pipe connection.
//!
//! All exchanges are serialized by mutable ownership. A failed or timed-out
//! exchange destroys the connection: a possibly committed request is never retried.
//! The failure is then recorded as a [`crate::failure::FailureReport`].
use std::fs::{self, File};
use std::io;
use std::path::PathBuf;

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
    output: tokio::sync::mpsc::Receiver<io::Result<Reply>>,
    events: tokio::sync::broadcast::Sender<serde_json::Value>,
    reader: tokio::task::JoinHandle<()>,
    session: String,
    next_request: u64,
    epoch: u64,
    state: SessionState,
    timeout: Duration,
    root_lock: Option<File>,
    #[cfg(windows)]
    job: Option<std::sync::Arc<crate::process_job::ProcessJob>>,
    /// Worker storage root: holds `worker.stderr.log`, `log/` and `failure.json`.
    root: PathBuf,
    last_method: Option<Method>,
    failure: Option<crate::failure::FailureReport>,
}

/// Terminates one worker's job without the exchange lock, e.g. while a stalled exchange
/// holds it. Holds no strong reference: once the worker releases its job this does nothing.
#[derive(Debug, Clone, Default)]
pub struct KillSwitch {
    #[cfg(windows)]
    job: std::sync::Weak<crate::process_job::ProcessJob>,
}

impl KillSwitch {
    /// End the worker and its contained descendants; false if it was already released.
    /// The in-flight exchange then fails on the closed pipe and the connection is marked
    /// crashed. Not available off Windows, where this returns false.
    pub fn terminate(&self) -> bool {
        #[cfg(windows)]
        if let Some(job) = self.job.upgrade() {
            job.terminate();
            return true;
        }
        false
    }
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
        crate::root_lock::try_lock(&root_lock).map_err(io::Error::other)?;
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
        let session = SessionUuid::generate().to_string();
        let (replies, output_receiver) = tokio::sync::mpsc::channel(16);
        let (events, _) = tokio::sync::broadcast::channel(256);
        let event_sender = events.clone();
        let expected_session = session.clone();
        let reader = tokio::spawn(async move {
            let mut output = output;
            loop {
                let frame = read_frame(&mut output).await;
                match frame {
                    Ok(value)
                        if value.get("notification") == Some(&serde_json::Value::Bool(true)) =>
                    {
                        if value["session"].as_str() != Some(expected_session.as_str())
                            || value["version_major"] != 1
                            || value["version_minor"] != 0
                            || !value["epoch"].is_u64()
                        {
                            let _ = replies
                                .send(Err(io::Error::other("invalid event identity")))
                                .await;
                            break;
                        }
                        let _ = event_sender.send(value);
                    }
                    value => {
                        let reply = value.and_then(|v| {
                            serde_json::from_value::<Reply>(v).map_err(io::Error::other)
                        });
                        let failed = reply.is_err();
                        if replies.send(reply).await.is_err() || failed {
                            break;
                        }
                    }
                }
            }
        });
        let mut worker = Self {
            child,
            input,
            output: output_receiver,
            events,
            reader,
            session,
            next_request: 1,
            epoch: 1,
            state: SessionState::Created,
            timeout,
            root_lock: Some(root_lock),
            #[cfg(windows)]
            job: Some(std::sync::Arc::new(job)),
            root: spec.storage_root.clone(),
            last_method: None,
            failure: None,
        };
        let hello = worker
            // Only stops are pushed: the GDB server is the one consumer (crates/gdb
            // wait_for_stop). Other categories stay readable through `events`/`logs`
            // (lotus#26). Workers before that change ignore `subscriptions` and push all.
            .command(
                Method::Hello,
                serde_json::json!({"notifications": true, "subscriptions": ["stopped"]}),
            )
            .await?;
        if hello.status != Status::Ok {
            return Err(io::Error::other("worker rejected handshake"));
        }
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

    #[must_use]
    pub fn kill_switch(&self) -> KillSwitch {
        KillSwitch {
            #[cfg(windows)]
            job: self
                .job
                .as_ref()
                .map(std::sync::Arc::downgrade)
                .unwrap_or_default(),
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<serde_json::Value> {
        self.events.subscribe()
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
            _ => Err(self
                .fail_with(io::Error::other("worker failed to exit cleanly"))
                .await),
        }
    }

    /// Why the connection failed, once it has.
    #[must_use]
    pub fn failure(&self) -> Option<&crate::failure::FailureReport> {
        self.failure.as_ref()
    }

    /// End the connection after `error` and record why. Logs are read after the process
    /// exited, bounded, off the async runtime. Returns `error` with the cause appended.
    async fn fail_with(&mut self, error: io::Error) -> io::Error {
        let pid = self.child.id();
        self.state = SessionState::Crashed;
        let _ = self.child.kill().await;
        let exit_code = self
            .child
            .wait()
            .await
            .ok()
            .and_then(|status| status.code());
        self.release_resources();
        let root = self.root.clone();
        let (log_tail, stderr_tail) = tokio::task::spawn_blocking(move || {
            (
                crate::failure::tail(&root.join("log").join("azahar_log.txt")).unwrap_or_default(),
                crate::failure::tail(&root.join("worker.stderr.log")).unwrap_or_default(),
            )
        })
        .await
        .unwrap_or_default();
        let (cause, marker) =
            crate::failure::classify(&log_tail, &stderr_tail, exit_code, error.kind());
        let report = crate::failure::FailureReport {
            cause,
            marker,
            transport_error: error.to_string(),
            exit_code,
            pid,
            connection: self.session.clone(),
            last_method: self.last_method.map(|method| format!("{method:?}")),
            last_request_id: self.next_request.saturating_sub(1),
            gpu: crate::failure::gpu_error(&stderr_tail),
            epoch: self.epoch,
            host_time_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |time| time.as_millis() as u64),
            stderr_tail,
            log_tail,
        };
        if let Ok(json) = serde_json::to_vec_pretty(&report) {
            let _ = fs::write(self.root.join("failure.json"), json);
        }
        let summary = report.summary();
        self.failure = Some(report);
        io::Error::new(error.kind(), format!("{error}; {summary}"))
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
        self.last_method = Some(method);
        let result = tokio::time::timeout(self.timeout, self.exchange(&request)).await;
        let reply = match result {
            Ok(Ok(reply)) => reply,
            Ok(Err(error)) => return Err(self.fail_with(error).await),
            Err(_) => {
                return Err(self
                    .fail_with(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "worker exchange deadline exceeded; result unknown",
                    ))
                    .await);
            }
        };
        if !lotus_protocol::is_compatible(reply.version_major, reply.version_minor)
            || reply.request_id != request.request_id
            || reply.session != self.session
            || reply.epoch
                != self.epoch
                    + u64::from(
                        (matches!(method, Method::Load | Method::Reset)
                            || (method == Method::Debug
                                && matches!(
                                    request.params["operation"].as_str(),
                                    Some(
                                        "load_state"
                                            | "citrace_replay"
                                            | "reverse_step"
                                            | "reverse_continue"
                                            | "boot_home_menu"
                                    )
                                )))
                            && reply.status == Status::Ok,
                    )
            || !matches!(
                reply.state.as_str(),
                "idle" | "running" | "paused" | "stopped"
            )
        {
            return Err(self
                .fail_with(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid worker reply identity, epoch, status or state",
                ))
                .await);
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
        self.output
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "worker output closed"))?
    }
}

impl Drop for ControlledWorker {
    fn drop(&mut self) {
        self.reader.abort();
        let _ = self.child.start_kill();
        self.release_resources();
    }
}

async fn read_frame(output: &mut ChildStdout) -> io::Result<serde_json::Value> {
    let length = output.read_u32_le().await?;
    control::validate_length(length)?;
    let mut payload = vec![0; length as usize];
    output.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).map_err(io::Error::other)
}
