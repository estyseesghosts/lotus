//! Trusted local builds (lotus#79).
//!
//! A build profile is host configuration: a project root, a devkitPro `make` or a CMake
//! build preset, and output paths relative to the root. Clients name a profile and never
//! supply a program, an argument or a path. The tool runs without a shell, in its own
//! Windows job object or Unix process group, with only the profile's environment plus
//! `SystemRoot`, `TEMP` and `TMP`. A successful build's outputs are copied into this host's
//! storage under the build's ID and hashed there, so a session keeps the exact files it
//! loaded while later builds of any profile run.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

const MAX_RUNNING: usize = 2;
const MAX_JOBS: usize = 32;
const LOG_LINES: usize = 4000;
const LOG_BYTES: usize = 512 << 10;
const LINE_BYTES: usize = 1024;
const PAGE_LINES: usize = 500;
const MAX_OUTPUT: u64 = 256 << 20;
const MAX_ELF: u64 = 128 << 20;
const MAX_MAP: u64 = 16 << 20;
/// Inherited only so Windows tools can run at all; everything else comes from `env`.
const INHERITED: [&str; 3] = ["SystemRoot", "TEMP", "TMP"];

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// One project's build, from host configuration only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BuildProfile {
    /// Project root; the tool runs here and every other path is relative to it.
    pub root: PathBuf,
    pub tool: BuildTool,
    /// Built content: a `.3dsx` (loaded by `relaunch_build`) or a `.cia` (installed).
    pub output: PathBuf,
    /// Companion ELF with DWARF, bound after a relaunch.
    #[serde(default)]
    pub elf: Option<PathBuf>,
    /// Optional linker map retained with the build.
    #[serde(default)]
    pub map: Option<PathBuf>,
    /// Source directory for `source_excerpt`, registered as source root `build:<name>`.
    #[serde(default)]
    pub sources: Option<PathBuf>,
    /// The tool's whole environment, apart from `SystemRoot`, `TEMP` and `TMP`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_timeout() -> u64 {
    600
}

/// The build command. Neither form passes through a shell.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildTool {
    /// `<program> [-j <jobs>] [targets…]`, e.g. devkitPro's msys2 `make`.
    Make {
        program: PathBuf,
        #[serde(default)]
        targets: Vec<String>,
        #[serde(default)]
        jobs: Option<u32>,
    },
    /// `<program> --build --preset <preset>`.
    CmakePreset { program: PathBuf, preset: String },
}

impl BuildTool {
    fn program(&self) -> &Path {
        match self {
            Self::Make { program, .. } | Self::CmakePreset { program, .. } => program,
        }
    }

    fn args(&self) -> Vec<String> {
        match self {
            Self::Make { targets, jobs, .. } => jobs
                .iter()
                .flat_map(|jobs| ["-j".to_string(), jobs.to_string()])
                .chain(targets.iter().cloned())
                .collect(),
            Self::CmakePreset { preset, .. } => {
                vec!["--build".into(), "--preset".into(), preset.clone()]
            }
        }
    }
}

/// A relative path that cannot leave the root: only normal components.
fn contained(key: &str, path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(invalid(format!(
            "{key} must be a relative path inside the project root"
        )));
    }
    Ok(())
}

/// A make target or CMake preset: no option (`-`), variable assignment (`=`) or shell text.
fn word(key: &str, text: &str) -> io::Result<()> {
    let ok = !text.is_empty()
        && text.len() <= 128
        && !text.starts_with('-')
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-/".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!("{key} {text:?} must be a plain name")))
    }
}

/// Check a configured profile and canonicalize its root and program.
pub(crate) fn validate(name: &str, profile: &BuildProfile) -> io::Result<BuildProfile> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(invalid(format!("build profile name {name:?}")));
    }
    let mut profile = profile.clone();
    profile.root = profile
        .root
        .canonicalize()
        .map_err(|e| invalid(format!("build_profiles[{name}].root: {e}")))?;
    if !profile.root.is_dir() {
        return Err(invalid(format!(
            "build_profiles[{name}].root is not a directory"
        )));
    }
    contained("output", &profile.output)?;
    let kind = profile
        .output
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if !matches!(kind.as_deref(), Some("3dsx" | "cia")) {
        return Err(invalid("build output must be a .3dsx or .cia file"));
    }
    if let Some(elf) = &profile.elf {
        contained("elf", elf)?;
    }
    if let Some(map) = &profile.map {
        contained("map", map)?;
    }
    if let Some(sources) = &profile.sources {
        contained("sources", sources)?;
    }
    let program = match &mut profile.tool {
        BuildTool::Make {
            program,
            targets,
            jobs,
        } => {
            if targets.len() > 16 {
                return Err(invalid("at most 16 make targets"));
            }
            for target in targets.iter() {
                word("make target", target)?;
            }
            if jobs.is_some_and(|jobs| !(1..=64).contains(&jobs)) {
                return Err(invalid("make jobs 1..64"));
            }
            program
        }
        BuildTool::CmakePreset { program, preset } => {
            word("CMake preset", preset)?;
            program
        }
    };
    *program = program
        .canonicalize()
        .map_err(|e| invalid(format!("build_profiles[{name}] program: {e}")))?;
    if !program.is_file() {
        return Err(invalid(format!(
            "build_profiles[{name}] program is not a file"
        )));
    }
    #[cfg(windows)]
    if !program
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return Err(invalid("build program must be an .exe on Windows"));
    }
    if profile.env.len() > 64
        || profile.env.iter().any(|(key, value)| {
            key.is_empty()
                || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || key.as_bytes()[0].is_ascii_digit()
                || value.contains('\0')
        })
    {
        return Err(invalid("build env: at most 64 NAME=value entries"));
    }
    if !(1..=3600).contains(&profile.timeout_secs) {
        return Err(invalid("build timeout_secs 1..3600"));
    }
    Ok(profile)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BuiltFile {
    /// Approved asset handle of the stored copy.
    pub asset: String,
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BuildRecord {
    pub job: u64,
    pub profile: String,
    /// `3dsx` or `cia`.
    pub kind: String,
    pub output: BuiltFile,
    pub elf: Option<BuiltFile>,
    pub map: Option<BuiltFile>,
    pub build_id: Option<String>,
    /// `DW_AT_producer` strings: compiler version and recorded flags.
    pub producers: Vec<String>,
    pub program: String,
    pub args: Vec<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone)]
enum State {
    Running,
    Succeeded(Box<BuildRecord>),
    Failed {
        reason: String,
        exit_code: Option<i32>,
    },
    Cancelled,
    TimedOut,
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded(_) => "succeeded",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
}

#[derive(Default)]
struct Log {
    lines: VecDeque<(u64, &'static str, String)>,
    bytes: usize,
    next: u64,
    dropped: u64,
}

impl Log {
    fn push(&mut self, stream: &'static str, mut line: String) {
        if line.len() > LINE_BYTES {
            let mut end = LINE_BYTES;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            line.truncate(end);
            line.push('\u{2026}');
        }
        self.bytes += line.len();
        self.lines.push_back((self.next, stream, line));
        self.next += 1;
        while self.lines.len() > LOG_LINES || self.bytes > LOG_BYTES {
            if let Some((_, _, old)) = self.lines.pop_front() {
                self.bytes -= old.len();
                self.dropped += 1;
            }
        }
    }
}

struct Job {
    id: u64,
    profile: String,
    started_ms: u64,
    finished_ms: Mutex<Option<u64>>,
    state: Mutex<State>,
    log: Mutex<Log>,
    cancel: tokio::sync::watch::Sender<bool>,
    finished: tokio::sync::Notify,
}

impl Job {
    fn summary(&self) -> Value {
        let state = lock(&self.state).clone();
        let finished = *lock(&self.finished_ms);
        let mut value = json!({
            "job": self.id,
            "profile": self.profile,
            "state": state.name(),
            "started_unix_ms": self.started_ms,
            "finished_unix_ms": finished,
        });
        match state {
            State::Succeeded(record) => value["build"] = json!(record),
            State::Failed { reason, exit_code } => {
                value["reason"] = reason.into();
                value["exit_code"] = exit_code.into();
            }
            State::TimedOut => value["reason"] = "the build exceeded its timeout".into(),
            _ => {}
        }
        value
    }
}

pub(crate) struct Builds {
    profiles: BTreeMap<String, BuildProfile>,
    /// This host's store; removed on close. Handles expire with the host.
    store: PathBuf,
    next: Mutex<u64>,
    closing: AtomicBool,
    jobs: Mutex<BTreeMap<u64, Arc<Job>>>,
}

impl Builds {
    pub(crate) fn new(profiles: BTreeMap<String, BuildProfile>, store: PathBuf) -> Self {
        Self {
            profiles,
            store,
            next: Mutex::new(0),
            closing: AtomicBool::new(false),
            jobs: Mutex::default(),
        }
    }

    pub(crate) fn cancel_all(&self) {
        let jobs = lock(&self.jobs);
        self.closing.store(true, Ordering::Release);
        for job in jobs.values() {
            job.cancel.send_replace(true);
        }
    }

    pub(crate) async fn close(&self) {
        let jobs: Vec<_> = lock(&self.jobs).values().cloned().collect();
        for job in jobs {
            let finished = job.finished.notified();
            if matches!(*lock(&job.state), State::Running) {
                finished.await;
            }
        }
        let store = self.store.clone();
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(store)).await;
    }

    /// The stored file behind a `build:<profile>:<job>[:elf|:map]` handle.
    pub(crate) fn resolve(&self, handle: &str) -> Option<PathBuf> {
        let rest = handle.strip_prefix("build:")?;
        let (profile, rest) = rest.split_once(':')?;
        let (id, kind) = rest.split_once(':').unwrap_or((rest, ""));
        let job = lock(&self.jobs).get(&id.parse().ok()?)?.clone();
        if job.profile != profile {
            return None;
        }
        let State::Succeeded(record) = &*lock(&job.state) else {
            return None;
        };
        let file = match kind {
            "" => &record.output,
            "elf" => record.elf.as_ref()?,
            "map" => record.map.as_ref()?,
            _ => return None,
        };
        Some(self.store.join(&record.profile).join(id).join(&file.file))
    }

    /// Asset handles of every successful build.
    pub(crate) fn assets(&self) -> Vec<String> {
        lock(&self.jobs)
            .values()
            .filter_map(|job| match &*lock(&job.state) {
                State::Succeeded(record) => Some(record.clone()),
                _ => None,
            })
            .flat_map(|record| {
                std::iter::once(record.output.asset.clone())
                    .chain(record.elf.map(|e| e.asset))
                    .chain(record.map.map(|map| map.asset))
            })
            .collect()
    }

    pub(crate) fn content_record(&self, asset: &str) -> Option<BuildRecord> {
        let (profile, id) = asset.strip_prefix("build:")?.split_once(':')?;
        let record = self.record(id.parse().ok()?).ok()?;
        (record.profile == profile && record.output.asset == asset).then_some(record)
    }

    pub(crate) fn record(&self, id: u64) -> io::Result<BuildRecord> {
        let job = lock(&self.jobs)
            .get(&id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no build job {id}")))?;
        let state = lock(&job.state).clone();
        match state {
            State::Succeeded(record) => Ok(*record),
            state => Err(invalid(format!("build job {id} is {}", state.name()))),
        }
    }

    /// Start a build. Successful artifacts remain available for the lifetime of the host.
    pub(crate) fn start(self: &Arc<Self>, name: &str) -> io::Result<Value> {
        let profile = self.profiles.get(name).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no build profile {name:?}"),
            )
        })?;
        let job = {
            let mut jobs = lock(&self.jobs);
            if self.closing.load(Ordering::Acquire) {
                return Err(io::Error::other("the build host is closing"));
            }
            let running: Vec<&Arc<Job>> = jobs
                .values()
                .filter(|job| matches!(*lock(&job.state), State::Running))
                .collect();
            if let Some(job) = running
                .iter()
                .find(|job| self.profiles[&job.profile].root == profile.root)
            {
                return Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    format!(
                        "project for profile {name} is already building as job {}",
                        job.id
                    ),
                ));
            }
            if running.len() >= MAX_RUNNING {
                return Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    format!("{MAX_RUNNING} builds are already running"),
                ));
            }
            while jobs.len() >= MAX_JOBS {
                let evict = jobs.iter().find(|(_, job)| {
                    !matches!(*lock(&job.state), State::Running | State::Succeeded(_))
                });
                let Some((&id, job)) = evict else {
                    return Err(io::Error::new(
                        io::ErrorKind::ResourceBusy,
                        format!("host build capacity reached ({MAX_JOBS} jobs)"),
                    ));
                };
                let _ = std::fs::remove_dir_all(self.store.join(&job.profile).join(id.to_string()));
                jobs.remove(&id);
            }
            let mut next = lock(&self.next);
            *next += 1;
            let (cancel, _) = tokio::sync::watch::channel(false);
            let job = Arc::new(Job {
                id: *next,
                profile: name.to_string(),
                started_ms: now_ms(),
                finished_ms: Mutex::new(None),
                state: Mutex::new(State::Running),
                log: Mutex::default(),
                cancel,
                finished: tokio::sync::Notify::new(),
            });
            jobs.insert(job.id, job.clone());
            job
        };
        let builds = self.clone();
        let task_job = job.clone();
        let task = std::thread::Builder::new()
            .name(format!("lotus-build-{}", job.id))
            .spawn(move || {
                let started = Instant::now();
                let state = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(builds.run(&task_job, &profile, started)),
                    Err(error) => State::Failed {
                        reason: format!("could not create a build runtime: {error}"),
                        exit_code: None,
                    },
                };
                *lock(&task_job.finished_ms) = Some(now_ms());
                let mut current = lock(&task_job.state);
                *current = if *task_job.cancel.borrow() {
                    State::Cancelled
                } else {
                    state
                };
                task_job.finished.notify_waiters();
            });
        if let Err(error) = task {
            *lock(&job.state) = State::Failed {
                reason: error.to_string(),
                exit_code: None,
            };
            job.finished.notify_waiters();
            return Err(error);
        }
        Ok(job.summary())
    }

    async fn run(&self, job: &Arc<Job>, profile: &BuildProfile, started: Instant) -> State {
        let mut cancelled = job.cancel.subscribe();
        if *cancelled.borrow() {
            return State::Cancelled;
        }
        let mut command = tokio::process::Command::new(profile.tool.program());
        command
            .args(profile.tool.args())
            .current_dir(&profile.root)
            .env_clear()
            .envs(
                INHERITED
                    .iter()
                    .filter_map(|key| Some((key, std::env::var_os(key)?))),
            )
            .envs(&profile.env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // CREATE_NO_WINDOW | CREATE_SUSPENDED: resumed once inside its job object.
        #[cfg(windows)]
        command.creation_flags(0x0800_0004);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return State::Failed {
                    reason: format!(
                        "could not start {}: {error}",
                        profile.tool.program().display()
                    ),
                    exit_code: None,
                }
            }
        };
        let pid = child.id().expect("new child has not been reaped");
        #[cfg(windows)]
        let containment = match crate::process_job::ProcessJob::attach_and_resume(pid) {
            Ok(containment) => containment,
            Err(error) => {
                let _ = child.kill().await;
                return State::Failed {
                    reason: format!("could not contain the build process: {error}"),
                    exit_code: None,
                };
            }
        };
        #[cfg(unix)]
        let containment = crate::process_group::ProcessGroup::new(pid);
        let readers = [
            child.stdout.take().map(|out| pump(job, "stdout", out)),
            child.stderr.take().map(|err| pump(job, "stderr", err)),
        ];
        let timeout = tokio::time::sleep(Duration::from_secs(profile.timeout_secs));
        let outcome = tokio::select! {
            status = child.wait() => Ok(status),
            _ = cancelled.wait_for(|cancel| *cancel) => Err(State::Cancelled),
            () = timeout => Err(State::TimedOut),
        };
        #[cfg(windows)]
        containment.terminate();
        #[cfg(unix)]
        containment.retire();
        if outcome.is_err() {
            let _ = child.wait().await;
        }
        for mut reader in readers.into_iter().flatten() {
            if tokio::time::timeout(Duration::from_secs(1), &mut reader)
                .await
                .is_err()
            {
                reader.abort();
                let _ = reader.await;
            }
        }
        let status = match outcome {
            Ok(status) => status,
            Err(state) => return state,
        };
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                return State::Failed {
                    reason: error.to_string(),
                    exit_code: None,
                }
            }
        };
        if !status.success() {
            return State::Failed {
                reason: "the build tool reported failure".into(),
                exit_code: status.code(),
            };
        }
        let store = self.store.join(&job.profile).join(job.id.to_string());
        let profile = profile.clone();
        let (id, name) = (job.id, job.profile.clone());
        let duration = started.elapsed().as_millis() as u64;
        match tokio::task::spawn_blocking(move || collect(&profile, &store, id, &name, duration))
            .await
        {
            Ok(Ok(record)) => State::Succeeded(Box::new(record)),
            Ok(Err(error)) => State::Failed {
                reason: error.to_string(),
                exit_code: status.code(),
            },
            Err(error) => State::Failed {
                reason: error.to_string(),
                exit_code: status.code(),
            },
        }
    }

    pub(crate) fn cancel(&self, id: u64) -> io::Result<Value> {
        let job = lock(&self.jobs)
            .get(&id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no build job {id}")))?;
        let state = lock(&job.state);
        if !matches!(*state, State::Running) {
            return Err(invalid(format!("build job {id} already finished")));
        }
        job.cancel.send_replace(true);
        Ok(json!({"job": id, "state": "cancelling"}))
    }

    /// Profiles and jobs, or one job with its log from line `log_from`.
    pub(crate) fn status(&self, job: Option<u64>, log_from: u64) -> io::Result<Value> {
        let Some(id) = job else {
            let profiles: Vec<Value> = self
                .profiles
                .iter()
                .map(|(name, profile)| {
                    json!({
                        "name": name,
                        "tool": match &profile.tool {
                            BuildTool::Make { .. } => "make",
                            BuildTool::CmakePreset { .. } => "cmake_preset",
                        },
                        "program": profile.tool.program(),
                        "args": profile.tool.args(),
                        "root": profile.root,
                        "output": profile.output,
                        "elf": profile.elf,
                        "map": profile.map,
                    })
                })
                .collect();
            let jobs: Vec<Value> = lock(&self.jobs).values().map(|job| job.summary()).collect();
            return Ok(json!({"profiles": profiles, "jobs": jobs}));
        };
        let job = lock(&self.jobs)
            .get(&id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no build job {id}")))?;
        let mut value = job.summary();
        let log = lock(&job.log);
        let lines: Vec<Value> = log
            .lines
            .iter()
            .filter(|(seq, _, _)| *seq >= log_from)
            .take(PAGE_LINES)
            .map(|(seq, stream, text)| json!({"line": seq, "stream": stream, "text": text}))
            .collect();
        value["log"] = json!({
            "lines": lines,
            "next": log.lines.iter().find(|(seq, _, _)| *seq >= log_from).map_or(log.next, |(seq, _, _)| (seq + PAGE_LINES as u64).min(log.next)),
            "dropped_lines": log.dropped,
        });
        Ok(value)
    }

    /// Output and ELF identity side by side, with function and section differences.
    pub(crate) fn compare(&self, a: u64, b: u64) -> io::Result<Value> {
        let (first, second) = (self.record(a)?, self.record(b)?);
        let load = |record: &BuildRecord| -> io::Result<Option<lotus_debugger::symbols::Symbols>> {
            let Some(elf) = &record.elf else {
                return Ok(None);
            };
            let path = self
                .store
                .join(&record.profile)
                .join(record.job.to_string())
                .join(&elf.file);
            lotus_debugger::symbols::Symbols::parse(&std::fs::read(path)?).map(Some)
        };
        let mut value = json!({
            "a": first,
            "b": second,
            "same_output": first.output.sha256 == second.output.sha256,
            "same_elf": first.elf.as_ref().map(|e| &e.sha256) == second.elf.as_ref().map(|e| &e.sha256),
            "same_producers": first.producers == second.producers,
            "output_bytes_delta": second.output.bytes as i64 - first.output.bytes as i64,
        });
        if let (Some(x), Some(y)) = (load(&first)?, load(&second)?) {
            let functions =
                |symbols: &lotus_debugger::symbols::Symbols| -> BTreeMap<String, (u32, u64)> {
                    symbols
                        .functions()
                        .map(|(n, s, z)| (n.to_string(), (s, z)))
                        .collect()
                };
            let (fx, fy) = (functions(&x), functions(&y));
            let list = |names: Vec<&String>| -> Value {
                json!({"count": names.len(), "names": names.into_iter().take(64).collect::<Vec<_>>()})
            };
            value["functions"] = json!({
                "added": list(fy.keys().filter(|n| !fx.contains_key(*n)).collect()),
                "removed": list(fx.keys().filter(|n| !fy.contains_key(*n)).collect()),
                "resized": list(fx.iter().filter(|(n, (_, s))| fy.get(*n).is_some_and(|(_, t)| t != s)).map(|(n, _)| n).collect()),
                "moved": list(fx.iter().filter(|(n, (a, _))| fy.get(*n).is_some_and(|(b, _)| b != a)).map(|(n, _)| n).collect()),
            });
            let sections = |symbols: &lotus_debugger::symbols::Symbols| -> BTreeMap<String, u32> {
                symbols
                    .sections()
                    .iter()
                    .map(|s| (s.name.clone(), s.size))
                    .collect()
            };
            let (sx, sy) = (sections(&x), sections(&y));
            value["sections"] = sx
                .keys()
                .chain(sy.keys())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .map(|name| json!({"name": name, "a": sx.get(name), "b": sy.get(name)}))
                .collect();
        }
        Ok(value)
    }
}

/// Copy one output stream into the job's bounded log.
fn pump(
    job: &Arc<Job>,
    stream: &'static str,
    source: impl AsyncRead + Unpin + Send + 'static,
) -> tokio::task::JoinHandle<()> {
    let job = Arc::downgrade(job);
    tokio::spawn(async move {
        let mut source = BufReader::new(source);
        let mut line = Vec::with_capacity(LINE_BYTES + 1);
        while let Ok(chunk) = source.fill_buf().await {
            let Some(job) = job.upgrade() else { break };
            let end = chunk.iter().position(|&byte| byte == b'\n');
            let len = end.unwrap_or(chunk.len());
            let keep = len.min(LINE_BYTES + 1 - line.len());
            line.extend_from_slice(&chunk[..keep]);
            if end.is_some() || (chunk.is_empty() && !line.is_empty()) {
                lock(&job.log).push(
                    stream,
                    String::from_utf8_lossy(&line)
                        .trim_end_matches('\r')
                        .to_string(),
                );
                line.clear();
            }
            if chunk.is_empty() {
                break;
            }
            source.consume(len + usize::from(end.is_some()));
        }
    })
}

fn copy_hashed(
    source: &Path,
    destination: &Path,
    limit: u64,
) -> io::Result<(String, u64, Vec<u8>)> {
    let mut data = Vec::new();
    std::fs::File::open(source)?
        .take(limit + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(io::Error::other(format!(
            "{} exceeds {} MiB",
            source.display(),
            limit >> 20
        )));
    }
    std::fs::write(destination, &data)?;
    Ok((
        format!("{:x}", Sha256::digest(&data)),
        data.len() as u64,
        data,
    ))
}

/// Copy a finished build's outputs into the store, refusing paths that leave the root.
fn collect(
    profile: &BuildProfile,
    store: &Path,
    job: u64,
    name: &str,
    duration_ms: u64,
) -> io::Result<BuildRecord> {
    let inside = |relative: &Path| -> io::Result<PathBuf> {
        let path = profile
            .root
            .join(relative)
            .canonicalize()
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("build output {}: {error}", relative.display()),
                )
            })?;
        if !path.starts_with(&profile.root) || !path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "build output {} resolves outside the project root",
                    relative.display()
                ),
            ));
        }
        Ok(path)
    };
    std::fs::create_dir_all(store)?;
    let output = inside(&profile.output)?;
    let file = profile
        .output
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::other("output has no file name"))?;
    let kind = Path::new(&file)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let (sha256, bytes, data) = copy_hashed(&output, &store.join(&file), MAX_OUTPUT)?;
    if kind == "3dsx" && (data.len() < 32 || !data.starts_with(b"3DSX")) {
        return Err(invalid("build output is missing its 3DSX header"));
    }
    drop(data);
    let mut build_id = None;
    let mut producers = Vec::new();
    let elf = match &profile.elf {
        Some(relative) => {
            let source = inside(relative)?;
            let elf_file = format!(
                "{}.elf",
                Path::new(&file)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            let elf_file = if elf_file == file {
                format!("{file}.elf")
            } else {
                elf_file
            };
            let (sha256, bytes, data) = copy_hashed(&source, &store.join(&elf_file), MAX_ELF)?;
            producers = lotus_debugger::symbols::producers(&data).unwrap_or_default();
            build_id = lotus_debugger::symbols::Symbols::parse(&data)
                .ok()
                .and_then(|symbols| symbols.build_id().map(str::to_owned));
            Some(BuiltFile {
                asset: format!("build:{name}:{job}:elf"),
                file: elf_file,
                sha256,
                bytes,
            })
        }
        None => None,
    };
    let map = if let Some(relative) = &profile.map {
        let source = inside(relative)?;
        let file = "linker.map".to_string();
        let (sha256, bytes, _) = copy_hashed(&source, &store.join(&file), MAX_MAP)?;
        Some(BuiltFile {
            asset: format!("build:{name}:{job}:map"),
            file,
            sha256,
            bytes,
        })
    } else {
        None
    };
    Ok(BuildRecord {
        job,
        profile: name.to_string(),
        kind,
        output: BuiltFile {
            asset: format!("build:{name}:{job}"),
            file,
            sha256,
            bytes,
        },
        elf,
        map,
        build_id,
        producers,
        program: profile.tool.program().display().to_string(),
        args: profile.tool.args(),
        duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(root: &Path, program: &Path) -> BuildProfile {
        BuildProfile {
            root: root.into(),
            tool: BuildTool::Make {
                program: program.into(),
                targets: vec!["all".into()],
                jobs: Some(2),
            },
            output: "out/app.3dsx".into(),
            elf: Some("out/app.elf".into()),
            map: None,
            sources: Some("source".into()),
            env: BTreeMap::from([("DEVKITPRO".into(), "C:/devkitPro".into())]),
            timeout_secs: 60,
        }
    }

    #[test]
    fn profiles_refuse_escapes_options_and_shell_text() {
        let root =
            std::env::temp_dir().join(format!("lotus-build-validate-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let program = std::env::current_exe().unwrap();
        let good = validate("app", &profile(&root, &program)).unwrap();
        assert_eq!(good.tool.args(), ["-j", "2", "all"]);
        for (name, broken) in [
            ("../escape", profile(&root, &program)),
            (
                "app",
                BuildProfile {
                    output: "../outside.3dsx".into(),
                    ..profile(&root, &program)
                },
            ),
            (
                "app",
                BuildProfile {
                    output: root.join("abs.3dsx"),
                    ..profile(&root, &program)
                },
            ),
            (
                "app",
                BuildProfile {
                    output: "out/app.exe".into(),
                    ..profile(&root, &program)
                },
            ),
            (
                "app",
                BuildProfile {
                    elf: Some("../../x.elf".into()),
                    ..profile(&root, &program)
                },
            ),
            (
                "app",
                BuildProfile {
                    sources: Some("..".into()),
                    ..profile(&root, &program)
                },
            ),
            (
                "app",
                BuildProfile {
                    tool: BuildTool::Make {
                        program: program.clone(),
                        targets: vec!["CC=evil".into()],
                        jobs: None,
                    },
                    ..profile(&root, &program)
                },
            ),
            (
                "app",
                BuildProfile {
                    tool: BuildTool::Make {
                        program: program.clone(),
                        targets: vec!["--eval=$(shell calc)".into()],
                        jobs: None,
                    },
                    ..profile(&root, &program)
                },
            ),
            (
                "app",
                BuildProfile {
                    tool: BuildTool::CmakePreset {
                        program: program.clone(),
                        preset: "x; rm -rf /".into(),
                    },
                    ..profile(&root, &program)
                },
            ),
            (
                "app",
                BuildProfile {
                    root: root.join("missing"),
                    ..profile(&root, &program)
                },
            ),
            (
                "app",
                BuildProfile {
                    env: BTreeMap::from([("BAD KEY".into(), "x".into())]),
                    ..profile(&root, &program)
                },
            ),
        ] {
            assert!(validate(name, &broken).is_err(), "{name}: {broken:?}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn build_handles_resolve_only_their_own_successful_job() {
        let builds = Builds::new(
            BTreeMap::new(),
            std::env::temp_dir().join("lotus-build-none"),
        );
        for handle in [
            "build:app:1",
            "build:app:1:elf",
            "build:app",
            "build::1",
            "build:app:1:map",
            "app",
        ] {
            assert!(builds.resolve(handle).is_none(), "{handle}");
        }
    }
}
