//! Native controller acceptance without a GUI automation dependency.
//!
//! Covers the session model end to end on real workers: a primary keeps its
//! card data, a secondary copied from it diverges and is deleted on stop,
//! sessions run concurrently, crash isolation holds, and an MCP server sharing
//! the controller's supervisor sees and drives the same sessions.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lotus_desktop::desktop_controller::{Command, DesktopController, McpMode, Update};
use lotus_mcp::server::{Frontend, LotusServer};
use lotus_mcp::tool_arguments::{AdvanceArgs, SessionArgs};
use lotus_protocol::envelope::SessionUuid;
use lotus_session::supervisor::{HostConfig, SessionKind};
use lotus_session::SessionState;
use rmcp::handler::server::wrapper::Parameters;

fn wait(controller: &DesktopController, what: &str, predicate: impl Fn(&Update) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(35);
    loop {
        {
            let state = controller.latest.lock().expect("controller snapshot");
            if predicate(&state) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "deadline waiting for {what}; last error: {}",
                state.error
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn session<'a>(
    update: &'a Update,
    id: &str,
) -> Option<&'a lotus_session::supervisor::SessionSummary> {
    update.sessions.iter().find(|s| s.info.id == id)
}

fn selected(controller: &DesktopController) -> String {
    controller.latest.lock().expect("snapshot").selected.clone()
}

#[test]
#[ignore = "requires external native worker and homebrew fixture"]
fn controller_runs_primary_and_secondary_sessions_shared_with_mcp() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").expect("native worker"));
    let fixture = PathBuf::from(std::env::var_os("LOTUS_FIXTURE").expect("fixture"));
    let root = std::env::temp_dir().join(format!("lotus-controller-{}", SessionUuid::generate()));
    let mut config = HostConfig::new(worker, root.clone());
    config.assets = BTreeMap::from([("fixture".into(), fixture)]);
    config.allow_control = true;
    let controller = DesktopController::start(config, McpMode::Off).expect("controller");
    let card = root.join("cards/dev");

    // Primary: created with content, autoplays, streams frames.
    controller
        .commands
        .send(Command::Create {
            kind: SessionKind::Primary { card: "dev".into() },
            asset: Some("fixture".into()),
        })
        .expect("create primary");
    wait(&controller, "primary", |state| state.sessions.len() == 1);
    let primary = selected(&controller);
    wait(&controller, "primary preview", |state| {
        state.preview.as_ref().is_some_and(|p| p.session == primary)
            && session(state, &primary).is_some_and(|s| s.telemetry.fps > 0.0)
    });
    std::fs::create_dir_all(card.join("sdmc")).expect("card sdmc");
    std::fs::write(card.join("sdmc/marker"), b"primary").expect("marker");

    // A second primary on the same card is refused.
    controller
        .commands
        .send(Command::Create {
            kind: SessionKind::Primary { card: "dev".into() },
            asset: None,
        })
        .expect("duplicate primary");
    wait(&controller, "duplicate refusal", |state| {
        state.error.contains("already has a primary")
    });

    // Secondary copied from the card runs concurrently in its own root.
    controller
        .commands
        .send(Command::Create {
            kind: SessionKind::Secondary {
                parent: Some("dev".into()),
            },
            asset: Some("fixture".into()),
        })
        .expect("create secondary");
    wait(&controller, "secondary", |state| {
        state.sessions.len() == 2 && state.selected != primary
    });
    let secondary = selected(&controller);
    let scratch = root.join("scratch").join(&secondary);
    assert_eq!(
        std::fs::read(scratch.join("sdmc/marker")).expect("copied card"),
        b"primary"
    );
    std::fs::write(scratch.join("sdmc/marker"), b"secondary").expect("diverge");
    wait(&controller, "both running", |state| {
        [&primary, &secondary].iter().all(|id| {
            session(state, id).is_some_and(|s| s.telemetry.playing && s.telemetry.frames > 0)
        })
    });

    // MCP over the same supervisor sees both and drives the secondary.
    let server = LotusServer::shared(
        controller.host().clone(),
        Frontend::Windowed,
        BTreeMap::new(),
    );
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let status = server.lotus_status().await;
        assert!(status.contains("\"frontend\":\"windowed\""));
        assert!(status.contains(&primary) && status.contains(&secondary));
        let advanced = server
            .lotus_run_until(Parameters(AdvanceArgs {
                session: secondary.clone(),
                frames: 10,
            }))
            .await;
        assert_ne!(advanced.is_error, Some(true), "{:?}", advanced.content);
        let focused = server
            .lotus_focus_session(Parameters(SessionArgs {
                session: primary.clone(),
            }))
            .await;
        assert_ne!(focused.is_error, Some(true));
    });
    wait(&controller, "agent pause and focus", |state| {
        state.selected == primary
            && session(state, &secondary).is_some_and(|s| {
                !s.telemetry.playing
                    && s.telemetry.state == SessionState::Paused
                    && s.telemetry.last_operation == "run_until 10"
            })
    });

    // Step, then crash isolation: aborting the secondary leaves the primary running.
    controller
        .commands
        .send(Command::Step(secondary.clone()))
        .expect("step");
    controller
        .commands
        .send(Command::Abort(secondary.clone()))
        .expect("inject failure");
    wait(&controller, "secondary crash", |state| {
        session(state, &secondary).is_some_and(|s| s.telemetry.state == SessionState::Crashed)
            && session(state, &primary).is_some_and(|s| s.telemetry.playing)
    });
    controller
        .commands
        .send(Command::Stop(secondary.clone()))
        .expect("dismiss crashed secondary");
    wait(&controller, "secondary removed", |state| {
        session(state, &secondary).is_none()
    });
    assert!(
        !scratch.join("sdmc").exists(),
        "secondary guest data discarded"
    );
    assert!(
        scratch.join("worker.stderr.log").exists(),
        "abnormal end keeps logs"
    );

    // Reset and stop the primary; its card persists.
    controller
        .commands
        .send(Command::Reset(primary.clone()))
        .expect("reset");
    wait(&controller, "reset", |state| {
        session(state, &primary).is_some_and(|s| !s.telemetry.playing && s.telemetry.frames == 0)
    });
    controller
        .commands
        .send(Command::Stop(primary.clone()))
        .expect("stop primary");
    wait(&controller, "primary stopped", |state| {
        state.sessions.is_empty()
    });
    assert_eq!(
        std::fs::read(card.join("sdmc/marker")).expect("persisted"),
        b"primary"
    );
    drop(controller);
    std::fs::remove_dir_all(root).expect("controller releases workers, logs and locks on close");
}
