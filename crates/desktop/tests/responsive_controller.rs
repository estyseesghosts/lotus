//! The desktop loop stays responsive while one worker stalls (#21), against
//! `lotus-mock-native` child processes (built by `cargo build -p lotus-session`).
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lotus_desktop::desktop_controller::{Command, DesktopController, McpMode, Update};
use lotus_protocol::envelope::SessionUuid;
use lotus_session::supervisor::{HostConfig, SessionKind};

fn mock_worker() -> PathBuf {
    let path = std::env::var_os("LOTUS_MOCK_NATIVE").map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/debug/lotus-mock-native.exe")
        },
        PathBuf::from,
    );
    assert!(
        path.is_file(),
        "{} missing: run `cargo build -p lotus-session` or set LOTUS_MOCK_NATIVE",
        path.display()
    );
    path
}

fn wait(controller: &DesktopController, what: &str, predicate: impl Fn(&Update) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if predicate(&controller.latest.lock().expect("snapshot")) {
            return;
        }
        assert!(Instant::now() < deadline, "deadline waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn ticks(controller: &DesktopController) -> u64 {
    controller.host().frontend_metrics()["ticks"]
        .as_u64()
        .unwrap_or(0)
}

#[test]
fn a_stalled_selected_worker_never_freezes_the_window_or_its_siblings() {
    let base = std::env::temp_dir().join(format!("lotus-responsive-{}", SessionUuid::generate()));
    std::fs::create_dir_all(&base).expect("base");
    let content = base.join("demo.3dsx");
    std::fs::write(&content, b"3DSX").expect("content");
    let mut config = HostConfig::new(mock_worker(), base.join("store"));
    config.allow_control = true;
    config.assets = BTreeMap::from([("demo".into(), content)]);
    let controller = DesktopController::start(config, McpMode::Off).expect("controller");
    for _ in 0..2 {
        controller
            .commands
            .send(Command::Create {
                kind: SessionKind::Secondary { parent: None },
                asset: Some("demo".into()),
            })
            .expect("create");
    }
    wait(&controller, "two playing sessions", |update| {
        update.sessions.len() == 2 && update.sessions.iter().all(|s| s.telemetry.playing)
    });
    let ids: Vec<String> = {
        let update = controller.latest.lock().expect("snapshot");
        update.sessions.iter().map(|s| s.info.id.clone()).collect()
    };
    let (a, b) = (ids[0].clone(), ids[1].clone());
    controller
        .commands
        .send(Command::Select(a.clone()))
        .expect("select a");
    controller.inspector.store(true, Ordering::Release);
    wait(&controller, "a selected", |update| update.selected == a);

    let root = base
        .join("store")
        .canonicalize()
        .expect("store")
        .join("scratch")
        .join(&a);
    std::fs::write(root.join("mock-stall-ms"), "4000").expect("stall");
    // The stalled reply now holds A; inspector and preview requests for A queue behind it.
    std::thread::sleep(Duration::from_millis(1300));
    wait(&controller, "the slow operation to be named", |update| {
        !update.waiting.is_empty()
    });

    let started = Instant::now();
    let before = ticks(&controller);
    controller
        .commands
        .send(Command::Select(b.clone()))
        .expect("select b");
    controller
        .commands
        .send(Command::Pause(b.clone()))
        .expect("pause b");
    wait(&controller, "b selected and paused", |update| {
        update.selected == b
            && update
                .sessions
                .iter()
                .any(|s| s.info.id == b && !s.telemetry.playing)
    });
    let responded = started.elapsed();
    assert!(responded < Duration::from_millis(500), "took {responded:?}");
    std::thread::sleep(Duration::from_millis(500));
    let published = ticks(&controller) - before;
    let window = started.elapsed();
    eprintln!(
        "{published} publications in {window:?} during the stall; {}",
        controller.host().frontend_metrics()
    );
    // The target is one per 33 ms (fewer on a loaded host, Windows timers round sleeps up);
    // a loop awaiting A would publish none until A answers 2.7 s later.
    assert!(published >= 5, "{published} publications during the stall");

    // After A answers, its late inspector result must not appear under B.
    std::thread::sleep(Duration::from_millis(3500));
    let update = controller.latest.lock().expect("snapshot");
    if let Some((session, _, _)) = &update.inspection {
        assert_eq!(session, &b, "stale inspection from A published under B");
    }
    let metrics = controller.host().frontend_metrics();
    assert!(
        metrics["stale_dropped"].as_u64().unwrap_or(0) >= 1,
        "{metrics}"
    );
    drop(update);
    drop(controller);
    let _ = std::fs::remove_dir_all(base);
}
