//! lotus#61: keyboard, pointer and controllers routed to two sessions at once, against the
//! real supervisor, `lotus-mock-native` workers (which log every accepted `input` to
//! `inputs.jsonl`) and the scripted controller backend. Found like
//! `responsive_controller.rs`: next to the profile directory or at `LOTUS_MOCK_NATIVE`.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lotus_desktop::input_routing::InputRouting;
use lotus_input::router::bits;
use lotus_input::synthetic::{Script, Synthetic};
use lotus_input::{Axis, Button, ControllerService, DeviceId, DeviceInfo, InputSnapshot};
use lotus_protocol::envelope::SessionUuid;
use lotus_session::native_session::InputState;
use lotus_session::supervisor::{HostConfig, SessionKind, Supervisor};
use lotus_session::telemetry::Actor;

fn mock_worker() -> PathBuf {
    let path = std::env::var_os("LOTUS_MOCK_NATIVE").map_or_else(
        || {
            std::env::current_exe()
                .expect("test executable")
                .parent()
                .and_then(Path::parent)
                .expect("profile directory")
                .join(format!("lotus-mock-native{}", std::env::consts::EXE_SUFFIX))
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

/// Buttons and circle pad of every input the worker accepted.
fn inputs(root: &Path) -> Vec<(u64, serde_json::Value)> {
    std::fs::read_to_string(root.join("inputs.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("json");
            (
                value["buttons"].as_u64().expect("buttons"),
                value["circle_pad"].clone(),
            )
        })
        .collect()
}

fn buttons(root: &Path) -> Vec<u64> {
    inputs(root).into_iter().map(|(b, _)| b).collect()
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "deadline waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn pad(script: &Script, handle: usize) {
    script.connect(
        handle,
        DeviceInfo {
            name: "Identical Pad".into(),
            model_uuid: "same-model".into(),
            vendor_id: Some(0x045e),
            product_id: Some(0x028e),
            mapped: true,
        },
        InputSnapshot::default(),
    );
}

#[test]
fn two_sessions_take_keyboard_and_controllers_independently() {
    let base = std::env::temp_dir().join(format!("lotus-routing-{}", SessionUuid::generate()));
    std::fs::create_dir_all(&base).expect("base");
    let content = base.join("demo.3dsx");
    std::fs::write(&content, b"3DSX").expect("content");
    let mut config = HostConfig::new(mock_worker(), base.join("store"));
    config.allow_control = true;
    config.assets = BTreeMap::from([("demo".into(), content)]);
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let host = Arc::new(Supervisor::new(config).expect("host"));
    let ids: Vec<String> = (0..2)
        .map(|_| {
            runtime.block_on(async {
                let info = host
                    .create(
                        SessionKind::Secondary { parent: None },
                        Some("demo"),
                        Actor::Host,
                    )
                    .await
                    .expect("create");
                host.run(&info.id, Actor::Host).await.expect("run");
                info.id
            })
        })
        .collect();
    let (a, b) = (ids[0].as_str(), ids[1].as_str());
    let store = base.join("store").canonicalize().expect("store");
    let root = |id: &str| store.join("scratch").join(id);

    let routing = InputRouting::new(host.clone());
    let (backend, script) = Synthetic::new();
    let (service, events) = ControllerService::start(move || Ok(backend)).expect("service");
    let service = Arc::new(service);
    let shutdown = Arc::new(AtomicBool::new(false));
    let thread = {
        let (routing, service, shutdown) = (routing.clone(), service.clone(), shutdown.clone());
        std::thread::spawn(move || routing.run(&service, &events, &shutdown))
    };

    // Two identical pads stay two connections; neither routes anywhere until assigned.
    pad(&script, 0);
    pad(&script, 1);
    until("two pads", || {
        routing.status()["assignments"].as_object().map(|m| m.len()) == Some(2)
    });
    let devices: Vec<DeviceId> = service.devices().iter().map(|d| d.id).collect();
    let (one, two) = (devices[0], devices[1]);
    script.button(0, Button::East, true);
    std::thread::sleep(Duration::from_millis(100));
    assert!(buttons(&root(a)).is_empty() && buttons(&root(b)).is_empty());
    script.button(0, Button::East, false);

    routing.assign(one, Some(a));
    routing.assign(two, Some(b));
    // Pad 2 holds B's A button while the keyboard plays session A.
    script.button(1, Button::East, true);
    until("pad 2 on B", || buttons(&root(b)) == [u64::from(bits::A)]);
    routing.keyboard(Some(a), bits::L);
    script.button(0, Button::South, true);
    until("keyboard and pad 1 on A", || {
        buttons(&root(a)).last() == Some(&u64::from(bits::L | bits::B))
    });
    // Keyboard focus loss releases L only; pad 1's B stays held.
    routing.keyboard(None, 0);
    until("keyboard released", || {
        buttons(&root(a)).last() == Some(&u64::from(bits::B))
    });
    // A stick on pad 1 reaches A's circle pad; B never saw any of A's traffic.
    script.axis(0, Axis::LeftX, -1.0);
    until("stick on A", || {
        inputs(&root(a)).last().map(|(_, c)| c.clone()) == Some(serde_json::json!([-1.0, 0.0]))
    });
    assert_eq!(buttons(&root(b)), [u64::from(bits::A)]);

    // Unplugging pad 2 releases B only; a held keyboard key on A survives it.
    routing.keyboard(Some(a), bits::R);
    script.disconnect(1);
    until("pad 2 released", || buttons(&root(b)).last() == Some(&0));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        buttons(&root(a)).last(),
        Some(&u64::from(bits::R | bits::B))
    );
    routing.keyboard(None, 0);

    // Reassigning pad 1 to B while it holds B and the stick: A is neutralized, B gets
    // nothing from the old hold.
    let before = buttons(&root(b)).len();
    routing.assign(one, Some(b));
    until("A neutral", || {
        inputs(&root(a)).last() == Some(&(0, serde_json::json!([0.0, 0.0])))
    });
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(buttons(&root(b)).len(), before, "held state leaked to B");
    script.button(0, Button::North, true);
    until("fresh press on B", || {
        buttons(&root(b)).last() == Some(&u64::from(bits::X))
    });

    // A stalled worker A does not hold up B's input.
    std::fs::write(root(a).join("mock-stall-ms"), "3000").expect("stall");
    std::thread::sleep(Duration::from_millis(300));
    routing.keyboard(Some(a), bits::Y);
    let started = Instant::now();
    script.button(0, Button::North, false);
    until("B during A's stall", || {
        buttons(&root(b)).last() == Some(&0)
    });
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    routing.keyboard(None, 0);
    until("A after its stall", || buttons(&root(a)).last() == Some(&0));

    // Scenario ownership (#48): live input to a scenario-owned worker is refused by the
    // worker and never replayed once control returns.
    host.submit_input(b, InputState::default(), "scenario", Actor::Mcp)
        .expect("scenario");
    script.button(0, Button::West, true);
    script.button(0, Button::West, false);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !buttons(&root(b)).contains(&u64::from(bits::Y)),
        "live input under scenario"
    );
    runtime.block_on(async {
        host.pause(b, Actor::Host).await.expect("pause");
        host.reset(b, Actor::Host).await.expect("reset");
        host.run(b, Actor::Host).await.expect("run");
    });
    let after_reset = buttons(&root(b)).len();
    script.button(0, Button::DpadUp, true);
    until("live after reset", || {
        buttons(&root(b))[after_reset..] == [u64::from(bits::UP)]
    });

    // Stopping B unassigns pad 1; its input goes nowhere afterwards.
    runtime.block_on(host.stop(b, Actor::Host)).expect("stop");
    until("pad 1 unassigned", || {
        routing.status()["assignments"][one.0.to_string()].is_null()
    });

    shutdown.store(true, std::sync::atomic::Ordering::Release);
    thread.join().expect("router thread");
    runtime.block_on(host.close());
    let _ = std::fs::remove_dir_all(&base);
}
