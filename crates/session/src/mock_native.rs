//! Mock of the native worker's control surface for supervisor tests; never emulates a console.
//!
//! Answers `hello`, `status`, `load`, `reset`, `run`, `run_until`, `pause`, `input` and
//! `stop` like `ctr_worker/control.h`: bounded runs advance `frames` at 600 per host second
//! and pause with stop reason `frame` at their target. Fault injection, all through files in
//! `LOTUS_STORAGE_ROOT` so a test can address one worker:
//! - `mock-stall-ms`: before the next reply, sleep that many milliseconds (file is consumed).
//! - every accepted `input` is appended to `inputs.jsonl` as `{"buttons","x","y","touch","owner"}`.
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
            Method::Run | Method::RunUntil => {
                if self.state != "paused" {
                    return (Status::NotReady, serde_json::Value::Null);
                }
                let frames = request.params["frames"].as_u64().unwrap_or(600);
                self.target = self.frames + frames;
                self.run_started = Some((Instant::now(), self.frames));
                self.stop_reason = None;
                self.state = "running";
                (Status::Ok, self.snapshot())
            }
            Method::Pause => {
                self.run_started = None;
                self.state = "paused";
                self.stop_reason = Some("manual");
                (Status::Ok, self.snapshot())
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
            Method::Stop => {
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
