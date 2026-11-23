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
//! - `mock-stall-ms`: before the next reply, sleep that many milliseconds (file is consumed);
//!   `<ms> <method>` (e.g. `5000 capture`) stalls only the next reply to that method.
//! - `mock-async-pending`: `flush_storage` answers `async_fs_pending` that many times.
//! - `mock-no-flush`: `flush_storage` and `analog_input` are unsupported and not listed in
//!   `features`, like a worker that predates them. `features` otherwise lists
//!   `flush_storage`, `gpu_capture_levels` (lotus#42) and `analog_input` (lotus#60).
//! - `capture` writes both screens to `frames/<screen>-<sequence % 3>.bgra` (LTRF header,
//!   solid colour by frame) like the native ring files.
//! - `mock-exit-code`: on the next request, exit with that code without replying.
//! - `mock-reject-debug`: `debug` answers `unsupported`; otherwise `rewind_configure` and
//!   `gpu_configure` are accepted and their params appended to `debug.jsonl`, one object
//!   per line.
//! - `debug` `settings_set` stores the value and `settings_list` returns every stored one
//!   (`debug_api` is true in hello); `status.renderer` is the stored `graphics_api`.
//! - `mock-backend-name`: its text is `backend_name` (default `mock-native`), so a test can pose
//!   as an AZAHAR or CITnLO worker for settings drift (lotus#50).
//! - `mock-gpu-memory`: its JSON is `status.gpu_memory` (lotus#46).
//! - `mock-gpu-error`: on the next request, write `worker_gpu_error: <its JSON>` to stderr and
//!   exit with 4, like `ExitOnGpuFailure` in `ctr_worker/control.h`.
//! - `mock-control-error`: on the next request, write `worker_control_error: <its text>` to
//!   stderr and exit with 1, like a control transport failure in `ctr_worker/control.h`.
//! - every accepted `input` is appended to `inputs.jsonl` as
//!   `{"buttons","x","y","touch","owner","circle_pad","c_stick"}` (sticks `null` when not sent).
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
    captures: u64,
    settings: serde_json::Map<String, serde_json::Value>,
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
            "backend_name": std::fs::read_to_string(self.root.join("mock-backend-name"))
                .unwrap_or_else(|_| "mock-native".into()),
            "source": "mock", "frames": self.frames,
            "stop": self.stop_reason.map(|reason| serde_json::json!({"reason": reason})),
            "last_error": "", "input_owner": self.owner, "buttons": self.buttons,
            "touch_down": self.touch, "audio_samples": 0, "audio_dropped": 0,
            "preview": "capture", "debug_api": true,
            "features": if self.root.join("mock-no-flush").exists() {
                serde_json::json!(["gpu_capture_levels"])
            } else {
                serde_json::json!(["flush_storage", "gpu_capture_levels", "analog_input", "input_boundary"])
            },
            "renderer": self.settings.get("graphics_api"),
            "gpu_memory": std::fs::read_to_string(self.root.join("mock-gpu-memory"))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok()),
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
                self.target = match request.params["frames"].as_u64() {
                    Some(frames) => self.frames.saturating_add(frames),
                    None if self.root.join("mock-continuous-run").exists() => u64::MAX,
                    None => self.frames + 600,
                };
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
                    "y": request.params["y"], "touch": self.touch, "owner": owner,
                    "circle_pad": request.params["circle_pad"], "c_stick": request.params["c_stick"]});
                if let Ok(mut log) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.root.join("inputs.jsonl"))
                {
                    // One write per line: `writeln!` issues several writes, and tests poll this
                    // file concurrently, so they could read a partial object.
                    let _ = log.write_all(
                        format!(
                            "{line}
"
                        )
                        .as_bytes(),
                    );
                }
                {
                    let mut reply = self.snapshot();
                    reply["input_frame"] = serde_json::json!(self.frames);
                    (Status::Ok, reply)
                }
            }
            Method::Debug => {
                let command = request.params["operation"].as_str().unwrap_or("");
                if self.root.join("mock-reject-debug").exists() {
                    return (Status::Unsupported, serde_json::Value::Null);
                }
                match command {
                    "rewind_configure" | "gpu_configure" => {
                        if let Ok(mut log) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(self.root.join("debug.jsonl"))
                        {
                            let _ = log.write_all(
                                format!(
                                    "{}
",
                                    request.params
                                )
                                .as_bytes(),
                            );
                        }
                    }
                    "settings_set" => {
                        let name = request.params["name"].as_str().unwrap_or("").to_string();
                        self.settings.insert(name, request.params["value"].clone());
                    }
                    "settings_list" => {
                        let settings: Vec<_> = self
                            .settings
                            .iter()
                            .map(|(name, value)| serde_json::json!({"name": name, "value": value}))
                            .collect();
                        return (Status::Ok, serde_json::json!({"settings": settings}));
                    }
                    _ => return (Status::Unsupported, serde_json::Value::Null),
                }
                (Status::Ok, serde_json::json!({"operation": command}))
            }
            Method::Capture => {
                self.advance();
                self.captures += 1;
                let sequence = self.captures;
                let mut frames = Vec::new();
                for (screen, width) in [("top", 400u32), ("bottom", 320u32)] {
                    let height = 240u32;
                    let bytes = (width * height * 4) as usize;
                    let path = format!("frames/{screen}-{}.bgra", sequence % 3);
                    let mut data = Vec::with_capacity(bytes + 32);
                    data.extend_from_slice(b"LTRF");
                    data.extend_from_slice(&1u32.to_le_bytes());
                    data.extend_from_slice(&self.epoch.to_le_bytes());
                    data.extend_from_slice(&sequence.to_le_bytes());
                    data.extend_from_slice(&width.to_le_bytes());
                    data.extend_from_slice(&height.to_le_bytes());
                    data.resize(bytes + 32, (self.frames % 256) as u8);
                    let _ = std::fs::create_dir_all(self.root.join("frames"));
                    if std::fs::write(self.root.join(&path), &data).is_err() {
                        return (Status::Unsupported, serde_json::Value::Null);
                    }
                    frames.push(serde_json::json!({"screen": screen, "path": path,
                        "epoch": self.epoch, "sequence": sequence, "slot": sequence % 3,
                        "width": width, "height": height, "stride": width * 4,
                        "format": "bgra8", "orientation": "top_left_row_major", "bytes": bytes}));
                }
                (Status::Ok, serde_json::json!({"frames": frames}))
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
        captures: 0,
        settings: serde_json::Map::new(),
    };
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        let request: Request = control::read(&mut input)?;
        let stall = root.join("mock-stall-ms");
        if let Ok(text) = std::fs::read_to_string(&stall) {
            // `<ms>` stalls the next reply; `<ms> <method>` the next reply to that method.
            let mut words = text.split_whitespace();
            let ms = words.next().and_then(|ms| ms.parse().ok()).unwrap_or(0);
            let method = words.next();
            let name = serde_json::to_value(request.method).unwrap_or_default();
            if method.is_none_or(|method| name == method) {
                let _ = std::fs::remove_file(&stall);
                std::thread::sleep(Duration::from_millis(ms));
            }
        }
        if let Ok(text) = std::fs::read_to_string(root.join("mock-gpu-error")) {
            eprintln!("worker_gpu_error: {}", text.trim());
            std::process::exit(4);
        }
        if let Ok(text) = std::fs::read_to_string(root.join("mock-control-error")) {
            eprintln!("worker_control_error: {}", text.trim());
            std::process::exit(1);
        }
        if let Ok(text) = std::fs::read_to_string(root.join("mock-exit-code")) {
            std::process::exit(text.trim().parse().unwrap_or(1));
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
