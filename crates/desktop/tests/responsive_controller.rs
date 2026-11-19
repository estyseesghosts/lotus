//! The desktop loop stays responsive while one worker stalls (#21), against
//! `lotus-mock-native` child processes, found next to this test's profile directory
//! (`cargo test --workspace` builds it) or at `LOTUS_MOCK_NATIVE`.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use lotus_desktop::desktop_controller::{Command, DesktopController, McpMode, Update};
use lotus_protocol::envelope::SessionUuid;
use lotus_session::supervisor::{HostConfig, SessionKind};

fn mock_worker() -> PathBuf {
    // target/<profile>/deps/<test>.exe -> target/<profile>/lotus-mock-native.exe
    let path = std::env::var_os("LOTUS_MOCK_NATIVE").map_or_else(
        || {
            std::env::current_exe()
                .expect("test executable")
                .parent()
                .and_then(std::path::Path::parent)
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
        assert_eq!(
            session.as_ref().map(|target| &target.id),
            Some(&b),
            "stale inspection from A published under B"
        );
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

fn root(base: &std::path::Path, id: &str) -> PathBuf {
    base.join("store")
        .canonicalize()
        .expect("store")
        .join("scratch")
        .join(id)
}

/// Reset keeps the session ID but starts a new guest epoch. A cached static
/// panel must disappear before the next request, rather than becoming new-guest data.
#[test]
fn reset_invalidates_cached_inspection_without_waiting_for_a_replacement_panel() {
    use lotus_desktop::inspection::Panel;
    let base = std::env::temp_dir().join(format!(
        "lotus-inspection-epoch-{}",
        SessionUuid::generate()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let content = base.join("demo.3dsx");
    std::fs::write(&content, b"3DSX").unwrap();
    let mut config = HostConfig::new(mock_worker(), base.join("store"));
    config.allow_control = true;
    config.assets = BTreeMap::from([("demo".into(), content)]);
    let controller = DesktopController::start(config, McpMode::Off).unwrap();
    controller
        .commands
        .send(Command::Create {
            kind: SessionKind::Secondary { parent: None },
            asset: Some("demo".into()),
        })
        .unwrap();
    wait(&controller, "created session", |u| !u.selected.is_empty());
    let id = controller.latest.lock().unwrap().selected.clone();
    controller
        .commands
        .send(Command::Pause(id.clone()))
        .unwrap();
    *controller.inspection_selection.lock().unwrap() = (Panel::Memory, 0);
    controller.inspector.store(true, Ordering::Release);
    wait(&controller, "initial memory snapshot", |u| {
        u.inspection.as_ref().is_some_and(|(target, panel, _)| {
            target.as_ref().is_some_and(|t| t.id == id) && *panel == Panel::Memory
        })
    });
    let before = controller
        .latest
        .lock()
        .unwrap()
        .inspection
        .as_ref()
        .unwrap()
        .0
        .clone()
        .unwrap();
    // Result has no polling command. No replacement can hide a stale cache.
    *controller.inspection_selection.lock().unwrap() = (Panel::Result, 0);
    controller
        .commands
        .send(Command::Reset(id.clone()))
        .unwrap();
    wait(&controller, "new guest epoch", |u| {
        u.sessions
            .iter()
            .any(|s| s.info.id == id && s.telemetry.epoch != before.epoch)
    });
    {
        let update = controller.latest.lock().unwrap();
        assert!(update.inspection.is_none(), "old epoch remained published");
        assert!(update
            .registers
            .as_ref()
            .is_none_or(|(target, _)| target.epoch != before.epoch));
    }
    *controller.inspection_selection.lock().unwrap() = (Panel::Memory, 0);
    wait(&controller, "new memory snapshot", |u| {
        u.inspection.as_ref().is_some_and(|(target, _, _)| {
            target.as_ref().is_some_and(|t| {
                t.id == id && t.epoch != before.epoch && t.resolve(&u.sessions).is_some()
            })
        })
    });
    drop(controller);
    assert!(base.is_absolute() && base.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(base).unwrap();
}

/// lotus#45: with A's preview and inspector exchanges stalled, B's first preview and
/// inspection arrive without waiting for A, and A's late replies never appear as B's.
#[test]
fn a_stalled_preview_and_inspector_do_not_delay_the_next_selection() {
    let base = std::env::temp_dir().join(format!("lotus-hol-{}", SessionUuid::generate()));
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
    controller.inspector.store(true, Ordering::Release);
    controller
        .commands
        .send(Command::Select(a.clone()))
        .expect("select a");
    wait(&controller, "a live preview of A", |update| {
        update.previews.contains_key(&a)
    });
    // A's next capture stalls 5 s; its preview task holds the exchange.
    std::fs::write(root(&base, &a).join("mock-stall-ms"), "5000 capture").expect("stall");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !controller.host().frontend_metrics()["pending"]
        .as_array()
        .is_some_and(|pending| {
            pending.iter().any(|op| {
                op["kind"] == "preview"
                    && op["session"] == a.as_str()
                    && op["age_ms"].as_f64().unwrap_or(0.0) > 200.0
            })
        })
    {
        assert!(
            Instant::now() < deadline,
            "A's preview never stalled: {}",
            controller.host().frontend_metrics()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let started = Instant::now();
    controller
        .commands
        .send(Command::Select(b.clone()))
        .expect("select b");
    wait(&controller, "B's first preview", |update| {
        update.previews.contains_key(&b)
    });
    let preview_latency = started.elapsed();
    wait(&controller, "B's first inspection", |update| {
        update
            .inspection
            .as_ref()
            .is_some_and(|(session, _, _)| session.as_ref().is_some_and(|target| target.id == b))
    });
    let inspection_latency = started.elapsed();
    eprintln!(
        "B preview {preview_latency:?}, inspection {inspection_latency:?}; {}",
        controller.host().frontend_metrics()
    );
    // A 5 s stall on A; a loop awaiting A would take ~4.8 s here.
    assert!(
        preview_latency < Duration::from_millis(1000),
        "{preview_latency:?}"
    );
    assert!(
        inspection_latency < Duration::from_millis(1000),
        "{inspection_latency:?}"
    );
    // A -> B -> A -> B while A is still stalled: never a second exchange of a kind on A.
    for target in [&a, &b, &a, &b] {
        controller
            .commands
            .send(Command::Select(target.clone()))
            .expect("select");
        std::thread::sleep(Duration::from_millis(60));
        let metrics = controller.host().frontend_metrics();
        for kind in ["preview", "inspection", "registers"] {
            let on_a = metrics["pending"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|op| op["kind"] == kind && op["session"] == a.as_str())
                .count();
            assert!(on_a <= 1, "{kind}: {metrics}");
        }
    }
    // After A answers, its result is dropped: everything shown is B's.
    std::thread::sleep(Duration::from_millis(5500));
    {
        let update = controller.latest.lock().expect("snapshot");
        assert_eq!(update.selected, b);
        assert!(
            !update.previews.contains_key(&a),
            "A's late preview shown after A left the view"
        );
        if let Some((session, _, _)) = &update.inspection {
            assert_eq!(session.as_ref().map(|target| &target.id), Some(&b));
        }
    }
    let metrics = controller.host().frontend_metrics();
    assert!(
        metrics["stale_dropped"].as_u64().unwrap_or(0) >= 1,
        "{metrics}"
    );
    assert!(
        metrics["pending"]
            .as_array()
            .unwrap()
            .iter()
            .all(|op| op["age_ms"].as_f64().unwrap_or(0.0) < 2000.0),
        "leaked operation: {metrics}"
    );
    drop(controller);
    let _ = std::fs::remove_dir_all(base);
}

/// lotus#65: both tiles of a split get their own previews. With A's capture stalled 5 s,
/// B's preview keeps advancing; selecting A does not collapse the split; untiling A
/// leaves both workers running and A's late preview is never published.
#[test]
fn tiled_sessions_preview_independently_while_one_stalls() {
    use lotus_session::workspace::{Layout, Operation};
    let base = std::env::temp_dir().join(format!("lotus-tiles-{}", SessionUuid::generate()));
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
        .send(Command::Workspace(Operation::Tile {
            first: a.clone(),
            second: b.clone(),
            axis: None,
            ratio: None,
            screens: None,
        }))
        .expect("tile");
    wait(&controller, "a split of A and B", |update| {
        update.workspace.layout.visible() == [a.as_str(), b.as_str()]
    });
    wait(&controller, "previews of both tiles", |update| {
        update.previews.contains_key(&a) && update.previews.contains_key(&b)
    });

    std::fs::write(root(&base, &a).join("mock-stall-ms"), "5000 capture").expect("stall");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !controller.host().frontend_metrics()["pending"]
        .as_array()
        .is_some_and(|pending| {
            pending.iter().any(|op| {
                op["kind"] == "preview"
                    && op["session"] == a.as_str()
                    && op["age_ms"].as_f64().unwrap_or(0.0) > 200.0
            })
        })
    {
        assert!(Instant::now() < deadline, "A's preview never stalled");
        std::thread::sleep(Duration::from_millis(20));
    }
    let sequence = |id: &str| controller.latest.lock().expect("snapshot").previews[id].sequence;
    let (a_before, b_before) = (sequence(&a), sequence(&b));
    controller
        .commands
        .send(Command::Select(a.clone()))
        .expect("select a");
    std::thread::sleep(Duration::from_millis(1000));
    let (a_after, b_after) = (sequence(&a), sequence(&b));
    eprintln!(
        "B previews during A's stall: {}; {}",
        b_after - b_before,
        controller.host().frontend_metrics()
    );
    assert_eq!(
        a_after, a_before,
        "A published while its capture was stalled"
    );
    // About 30 per second at the 33 ms tick; one stalled tile would give none if it blocked.
    assert!(
        b_after - b_before >= 5,
        "B advanced {} times",
        b_after - b_before
    );
    {
        let update = controller.latest.lock().expect("snapshot");
        assert_eq!(update.selected, a);
        assert!(
            matches!(update.workspace.layout, Layout::Split { .. }),
            "selection collapsed the split"
        );
    }

    controller
        .commands
        .send(Command::Workspace(Operation::Untile {
            keep: Some(b.clone()),
        }))
        .expect("untile");
    controller
        .commands
        .send(Command::Select(b.clone()))
        .expect("select b");
    wait(&controller, "B alone", |update| {
        update.workspace.layout.visible() == [b.as_str()]
    });
    // A answers its stalled capture after it left the view; that preview is dropped.
    std::thread::sleep(Duration::from_millis(5000));
    {
        let update = controller.latest.lock().expect("snapshot");
        assert!(
            !update.previews.contains_key(&a),
            "A's late preview published"
        );
        assert!(update.previews.contains_key(&b));
        assert_eq!(update.sessions.len(), 2, "untiling stopped a worker");
        assert!(update.sessions.iter().all(|s| s.telemetry.playing));
    }
    drop(controller);
    let _ = std::fs::remove_dir_all(base);
}

/// lotus#71: a tile's inspector targets that tile's session without selecting it, follows
/// it through a swap, and returns to the selection once the tile leaves the split.
#[test]
fn tile_inspector_targets_its_own_session_and_never_changes_selection() {
    use lotus_session::workspace::{Layout, Operation};
    let base = std::env::temp_dir().join(format!("lotus-tinsp-{}", SessionUuid::generate()));
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
    let send = |command| controller.commands.send(command).expect("command");
    send(Command::Select(a.clone()));
    // Outside a split a tile request is ignored: the inspector follows the selection.
    send(Command::Inspect(Some(b.clone())));
    wait(&controller, "inspector on the selection", |update| {
        update.inspected == a && update.inspect_tile.is_none()
    });
    send(Command::Workspace(Operation::Tile {
        first: a.clone(),
        second: b.clone(),
        axis: None,
        ratio: None,
        screens: None,
    }));
    wait(&controller, "a split of A and B", |update| {
        update.workspace.layout.visible() == [a.as_str(), b.as_str()]
    });
    controller.inspector.store(true, Ordering::Release);
    send(Command::Inspect(Some(b.clone())));
    wait(
        &controller,
        "B's inspection while A stays selected",
        |update| {
            update.inspected == b
                && update.inspection.as_ref().is_some_and(|(session, _, _)| {
                    session.as_ref().is_some_and(|target| target.id == b)
                })
        },
    );
    assert_eq!(controller.latest.lock().expect("snapshot").selected, a);
    send(Command::Workspace(Operation::Swap));
    wait(&controller, "the swapped split", |update| {
        update.workspace.layout.visible() == [b.as_str(), a.as_str()]
    });
    std::thread::sleep(Duration::from_millis(300));
    {
        let update = controller.latest.lock().expect("snapshot");
        assert_eq!(
            update.inspected, b,
            "the inspector follows its session, not the tile slot"
        );
        assert_eq!(update.selected, a);
    }
    send(Command::Workspace(Operation::Untile {
        keep: Some(a.clone()),
    }));
    wait(
        &controller,
        "B's inspector closed with its tile",
        |update| {
            matches!(update.workspace.layout, Layout::Single { .. })
                && update.inspect_tile.is_none()
                && update.inspected == a
        },
    );
    wait(&controller, "A's inspection after B left", |update| {
        update
            .inspection
            .as_ref()
            .is_some_and(|(session, _, _)| session.as_ref().is_some_and(|target| target.id == a))
    });
    let update = controller.latest.lock().expect("snapshot");
    assert!(
        update.sessions.iter().all(|s| s.telemetry.playing),
        "a worker stopped"
    );
    drop(update);
    drop(controller);
    let _ = std::fs::remove_dir_all(&base);
}

/// An MCP `single` survives the desktop's selection sync until the user selects again (#112).
#[test]
fn an_mcp_single_view_is_not_reverted_by_the_selection() {
    use lotus_session::telemetry::Actor;
    use lotus_session::workspace::Operation;
    let base = std::env::temp_dir().join(format!("lotus-msingle-{}", SessionUuid::generate()));
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
    // A create selects its session when it finishes; wait for both.
    wait(&controller, "two sessions created", |update| {
        update.sessions.len() == 2 && update.pending == 0
    });
    let ids: Vec<String> = {
        let update = controller.latest.lock().expect("snapshot");
        update.sessions.iter().map(|s| s.info.id.clone()).collect()
    };
    let (a, b) = (ids[0].clone(), ids[1].clone());
    // Show B first: A may already be shown by its create, and then "A shown" could pass
    // before Select(A) is consumed, leaving its reveal to fire after the MCP change.
    controller
        .commands
        .send(Command::Select(b.clone()))
        .expect("select");
    wait(&controller, "B shown", |update| {
        update.selected == b && update.workspace.layout.visible() == [b.as_str()]
    });
    controller
        .commands
        .send(Command::Select(a.clone()))
        .expect("select");
    wait(&controller, "A shown", |update| {
        update.selected == a && update.workspace.layout.visible() == [a.as_str()]
    });
    let host = controller.host();
    let revision = host.workspace().snapshot.revision;
    let committed = host
        .update_workspace(
            &Operation::Single { session: b.clone() },
            Some(revision),
            Actor::Mcp,
        )
        .expect("single(B)");
    let start = ticks(&controller);
    let deadline = Instant::now() + Duration::from_secs(10);
    while ticks(&controller) < start + 10 {
        assert!(Instant::now() < deadline, "the loop stopped ticking");
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = host.workspace().snapshot;
    assert_eq!(
        status.layout.visible(),
        [b.as_str()],
        "reverted to the selection"
    );
    assert_eq!(
        status.revision, committed.revision,
        "needless revision bump"
    );
    {
        let update = controller.latest.lock().expect("snapshot");
        assert_eq!(
            update.selected, a,
            "the workspace never changes the selection"
        );
        assert_eq!(update.workspace.layout.visible(), [b.as_str()]);
    }
    controller
        .commands
        .send(Command::Select(a.clone()))
        .expect("select");
    wait(&controller, "selecting A again shows it", |update| {
        update.workspace.layout.visible() == [a.as_str()]
    });
    drop(controller);
    let _ = std::fs::remove_dir_all(&base);
}
