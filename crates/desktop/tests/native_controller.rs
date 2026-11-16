//! Native controller acceptance without a GUI automation dependency.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use lotus_desktop::desktop_controller::{Command, DesktopController, Update};
use lotus_protocol::envelope::SessionUuid;
use lotus_session::supervisor::HostConfig;

fn wait(controller: &DesktopController, predicate: impl Fn(&Update) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(35);
    loop {
        {
            let state = controller.latest.lock().expect("controller snapshot");
            if predicate(&state) {
                return;
            }
        }
        assert!(Instant::now() < deadline, "controller acceptance deadline");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires external native worker and homebrew fixture"]
fn controller_streams_switches_resets_and_stops_native_sessions() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").expect("native worker"));
    let fixture = PathBuf::from(std::env::var_os("LOTUS_FIXTURE").expect("fixture"));
    let root = std::env::temp_dir().join(format!("lotus-controller-{}", SessionUuid::generate()));
    let controller = DesktopController::start(HostConfig {
        worker,
        storage_root: root.clone(),
        assets: BTreeMap::from([("fixture".into(), fixture)]),
        allow_control: true,
    })
    .expect("background controller");
    controller
        .commands
        .send(Command::Create("fixture".into()))
        .expect("create first");
    wait(&controller, |state| state.sessions.len() == 1);
    let first = controller.latest.lock().expect("snapshot").selected.clone();
    controller
        .commands
        .send(Command::Play(first.clone()))
        .expect("play first");
    wait(&controller, |state| {
        state
            .sessions
            .get(&first)
            .is_some_and(|session| session.images.is_some())
    });
    controller
        .commands
        .send(Command::Create("fixture".into()))
        .expect("create second");
    wait(&controller, |state| {
        state.sessions.len() == 2 && state.selected != first
    });
    let second = controller.latest.lock().expect("snapshot").selected.clone();
    controller
        .commands
        .send(Command::Play(second.clone()))
        .expect("play second");
    wait(&controller, |state| {
        state
            .sessions
            .get(&second)
            .is_some_and(|session| session.images.is_some())
    });
    controller
        .commands
        .send(Command::Select(first.clone()))
        .expect("switch session");
    wait(&controller, |state| state.selected == first);
    controller
        .commands
        .send(Command::Abort(second.clone()))
        .expect("inject worker failure");
    wait(&controller, |state| {
        state
            .sessions
            .get(&second)
            .is_some_and(|session| session.state == "crashed" && session.images.is_none())
    });
    controller
        .commands
        .send(Command::Reset(first.clone()))
        .expect("reset selected");
    wait(&controller, |state| {
        state
            .sessions
            .get(&first)
            .is_some_and(|session| session.frame == 0 && !session.playing)
    });
    controller
        .commands
        .send(Command::Stop(first.clone()))
        .expect("stop selected");
    wait(&controller, |state| !state.sessions.contains_key(&first));
    drop(controller); // Releases the crashed second worker and all locks before returning.
    std::fs::remove_dir_all(root).expect("controller releases workers, logs and locks on close");
}
