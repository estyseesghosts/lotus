//! Lists connected controllers and prints their events as JSON lines (lotus#59 harness).
//! `lotus-pads [seconds]` runs for the given time (default 10) and then prints diagnostics.
//! Under the default `wgi` backend events need a focused window of this process; a console
//! window may not count (gilrs README), so qualify with the `xinput` build as well.

use std::time::{Duration, Instant};

use lotus_input::gilrs_backend::GilrsBackend;
use lotus_input::ControllerService;

fn main() -> std::io::Result<()> {
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let (service, events) = ControllerService::start(GilrsBackend::new)?;
    let end = Instant::now() + Duration::from_secs(seconds);
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        if let Ok(event) = events.recv_timeout(left) {
            let line = serde_json::json!({"device": event.device, "event": event.kind});
            println!("{line}");
        }
    }
    let summary = serde_json::json!({
        "backend": if cfg!(feature = "xinput") { "xinput" } else { "wgi" },
        "devices": service.devices(),
        "diagnostics": service.diagnostics(),
    });
    println!("{summary}");
    Ok(())
}
