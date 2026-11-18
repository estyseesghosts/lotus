//! Mock of the native worker's control surface for supervisor tests; never emulates a console.
//!
//! Answers `hello`, `status`, `load`, `reset`, `run`, `run_until`, `pause`, `input`,
//! `flush_storage`, `registers` (only `guest_ticks`, 4,481,136 per frame as in
//! [`crate::telemetry::GUEST_REFRESH_HZ`]) and `stop` like `ctr_worker/control.h`: `run` advances `frames` at 600 per
//! host second and pauses with stop reason `frame` at its target; `run_until` completes before
//! replying. With a `mock-guest` file in the root, a "guest" thread rewrites `sdmc/a.bin` then
//! `sdmc/b.bin` with the same counter while running, so any instant on disk has `a == b` or
//! `a == b + 1`; pausing joins it.
//! Fault injection, all through files in `LOTUS_STORAGE_ROOT` so a test can address one worker:
//! - `mock-stall-ms`: before the next reply, sleep that many milliseconds (file is consumed).
//! - `mock-async-pending`: `flush_storage` answers `async_fs_pending` that many times.
//! - `mock-no-flush`: `flush_storage` is unsupported, like a worker that predates it.
//! - `mock-reject-debug`: `debug` answers `unsupported`; otherwise `rewind_configure` and
//!   `gpu_configure` are accepted.
//! - every accepted `input` is appended to `inputs.jsonl` as `{"buttons","x","y","touch","owner"}`.
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The simulated guest's card writes while running.
struct Guest {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

fn start_guest(root: &std::path::Path) -> Guest {
    let stop = Arc::new(AtomicBool::new(false));
    let sdmc = root.join("sdmc");
    let _ = std::fs::create_dir_all(&sdmc);
    let flag = stop.clone();
    let thread = std::thread::spawn(move || {
        let start = std::fs::read(sdmc.join("b.bin"))
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .map_or(0, u64::from_le_bytes);
        let mut counter = start;
        while !flag.load(Ordering::Acquire) {
            counter += 1;
            let _ = std::fs::write(sdmc.join("a.bin"), counter.to_le_bytes());
            let _ = std::fs::write(sdmc.join("b.bin"), counter.to_le_bytes());
        }
    });
    Guest { stop, thread }
}

fn stop_guest(guest: &mut Option<Guest>) {
    if let Some(guest) = guest.take() {
        guest.stop.store(true, Ordering::Release);
        let _ = guest.thread.join();
    }
}

use lotus_protocol::control::{self, Method, Reply, Request, Status};

struct Machine {
    root: PathBuf,
    session: Option<String>,
    epoch: u64,
    state: &'static str,
    frames: u64,
    target: u64,
    run_started: Option<(Instant, u64)>,
    stop_reason: Option<&'static str>,
    owner: String,
    buttons: u64,
    touch: bool,
    guest: Option<Guest>,
}

impl Machine {
    /// Advance a running guest to the frame implied by elapsed host time.
    fn advance(&mut self) {
        if let Some((started, base)) = self.run_started {
            let elapsed = started.elapsed().as_millis() as u64 * 600 / 1000;
            self.frames = (base + elapsed).min(self.target);
            if self.frames >= self.target {
                self.state = "paused";
                self.run_started = None;
                self.stop_reason = Some("frame");
                stop_guest(&mut self.guest);
            }
        }
    }

    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "backend_name": "mock-native", "source": "mock", "frames": self.frames,
            "stop": self.stop_reason.map(|reason| serde_json::json!({"reason": reason})),
            "last_error": "", "input_owner": self.owner, "buttons": self.buttons,
            "touch_down": self.touch, "audio_samples": 0, "audio_dropped": 0,
            "preview": "capture",
        })
    }

    fn handle(&mut self, request: &Request) -> (Status, serde_json::Value) {
        if self.session.is_none() && request.method != Method::Hello {
            return (Status::NotReady, serde_json::Value::Null);
        }
        if request.epoch != self.epoch {
            return (Status::StaleEpoch, serde_json::Value::Null);
        }
        self.advance();
        match request.method {
            Method::Hello => {
                self.session = Some(request.session.clone());
                (Status::Ok, self.snapshot())
            }
            Method::Status => (Status::Ok, self.snapshot()),
            Method::Load | Method::Reset => {
                if self.state == "running" {
                    return (Status::NotReady, serde_json::Value::Null);
                }
                self.epoch += 1;
                self.frames = 0;
                self.state = "paused";
                self.owner = "live".into();
                self.buttons = 0;
                self.touch = false;
                (Status::Ok, self.snapshot())
            }
            Method::Run => {
                if self.state != "paused" {
                    return (Status::NotReady, serde_json::Value::Null);
                }
                let frames = request.params["frames"].as_u64().unwrap_or(600);
                self.target = self.frames + frames;
                self.run_started = Some((Instant::now(), self.frames));
                self.stop_reason = None;
                self.state = "running";
                if self.root.join("mock-guest").exists() {
                    self.guest = Some(start_guest(&self.root));
                }
                (Status::Ok, self.snapshot())
            }
            Method::RunUntil => {
                if self.state != "paused" {
                    return (Status::NotReady, serde_json::Value::Null);
                }
                self.frames += request.params["frames"].as_u64().unwrap_or(1);
                self.stop_reason = Some("frame");
                (Status::Ok, self.snapshot())
            }
            Method::Pause => {
                self.run_started = None;
                self.state = "paused";
                self.stop_reason = Some("manual");
                stop_guest(&mut self.guest);
                (Status::Ok, self.snapshot())
            }
            Method::FlushStorage => {
                if self.state == "running" {
                    return (Status::NotReady, serde_json::json!({"reason": "running"}));
                }
                if self.root.join("mock-no-flush").exists() {
                    return (Status::Unsupported, serde_json::Value::Null);
                }
                let pending = self.root.join("mock-async-pending");
                if let Ok(text) = std::fs::read_to_string(&pending) {
                    let left: u32 = text.trim().parse().unwrap_or(0);
                    if left > 0 {
                        let _ = std::fs::write(&pending, (left - 1).to_string());
                        return (
                            Status::NotReady,
                            serde_json::json!({"reason": "async_fs_pending"}),
                        );
                    }
                }
                (Status::Ok, serde_json::json!({"flushed": true}))
            }
            Method::Input => {
                let owner = request.params["owner"].as_str().unwrap_or("");
                if self.owner == "scenario" && owner != "scenario" {
                    return (Status::PermissionDenied, serde_json::Value::Null);
                }
                self.owner = owner.to_string();
                self.buttons = request.params["buttons"].as_u64().unwrap_or(0);
                self.touch = request.params["touch"].as_bool().unwrap_or(false);
                let line = serde_json::json!({"buttons": self.buttons, "x": request.params["x"],
                    "y": request.params["y"], "touch": self.touch, "owner": owner});
                if let Ok(mut log) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.root.join("inputs.jsonl"))
                {
                    let _ = writeln!(log, "{line}");
                }
                (Status::Ok, self.snapshot())
            }
            Method::Debug => {
                let command = request.params["operation"].as_str().unwrap_or("");
                if self.root.join("mock-reject-debug").exists()
                    || !matches!(command, "rewind_configure" | "gpu_configure")
                {
                    return (Status::Unsupported, serde_json::Value::Null);
                }
                (Status::Ok, serde_json::json!({"operation": command}))
            }
            Method::Registers => (
                Status::Ok,
                serde_json::json!({"core": request.params["core"], "guest_ticks": self.frames * 4_481_136}),
            ),
            Method::Stop => {
                stop_guest(&mut self.guest);
                self.state = "stopped";
                (Status::Ok, serde_json::Value::Null)
            }
            _ => (Status::Unsupported, serde_json::Value::Null),
        }
    }
}

fn main() -> io::Result<()> {
    let root = PathBuf::from(std::env::var_os("LOTUS_STORAGE_ROOT").expect("supervisor root"));
    let mut machine = Machine {
        root: root.clone(),
        session: None,
        epoch: 1,
        state: "idle",
        frames: 0,
        target: 0,
        run_started: None,
        stop_reason: None,
        owner: "live".into(),
        buttons: 0,
        touch: false,
        guest: None,
    };
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let request: Request = control::read(&mut input)?;
        let stall = root.join("mock-stall-ms");
        if let Ok(text) = std::fs::read_to_string(&stall) {
            let _ = std::fs::remove_file(&stall);
            std::thread::sleep(Duration::from_millis(text.trim().parse().unwrap_or(0)));
        }
        let (status, data) = machine.handle(&request);
        let reply = Reply {
            version_major: lotus_protocol::PROTOCOL_VERSION_MAJOR,
            version_minor: lotus_protocol::PROTOCOL_VERSION_MINOR,
            request_id: request.request_id,
            session: request.session,
            epoch: machine.epoch,
            status,
            state: machine.state.into(),
            backend: "azahar_native_software".into(),
            data,
        };
        control::write(&mut output, &reply)?;
        output.flush()?;
        if machine.state == "stopped" {
            return Ok(());
        }
    }
}
