//! Opt-in native acceptance. Assets remain outside Git; no network or ROM download.
use std::path::PathBuf;

use lotus_protocol::envelope::SessionUuid;
use lotus_session::native_session::{InputState, NativeSession};
use lotus_session::supervisor::{HostConfig, Supervisor};

fn config() -> (PathBuf, PathBuf, PathBuf) {
    let executable =
        PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").expect("set LOTUS_NATIVE_WORKER"));
    let fixture = PathBuf::from(std::env::var_os("LOTUS_FIXTURE").expect("set LOTUS_FIXTURE"));
    let root = std::env::temp_dir().join(format!("lotus-native-{}", SessionUuid::generate()));
    (executable, fixture, root)
}

#[tokio::test]
#[ignore = "requires explicitly supplied native executable and homebrew fixture"]
async fn native_lifecycle_frames_input_and_safe_reads() {
    let (executable, fixture, root) = config();
    let mut session = NativeSession::spawn(&executable, &root)
        .await
        .expect("native handshake");
    assert!(
        NativeSession::spawn(&executable, &root).await.is_err(),
        "exclusive root lock"
    );
    session.load(&fixture).await.expect("load homebrew");
    session
        .input(
            InputState {
                buttons: 1,
                ..InputState::default()
            },
            "scenario",
        )
        .await
        .expect("exclusive input");
    assert!(session.input(InputState::default(), "live").await.is_err());
    session
        .input(InputState::default(), "scenario")
        .await
        .expect("release buttons");
    let status = session.advance(60).await.expect("bounded run");
    assert_eq!(status["frames"], 60);
    let frames = session.capture().await.expect("native screens");
    assert_eq!(frames[0].bgra.len(), 400 * 240 * 4);
    assert_eq!(frames[1].bgra.len(), 320 * 240 * 4);
    let lease = frames[0].lease.clone();
    for _ in 0..3 {
        session.capture().await.expect("replace ring slots");
    }
    assert!(
        session.read_lease(&lease).is_err(),
        "slot sequence invalidates lease"
    );
    session.run().await.expect("start asynchronous run");
    for _ in 0..10 {
        let registers = session
            .registers(0)
            .await
            .expect("safe read while running/limiting");
        assert_eq!(registers["safe_point"], true);
        let pc = registers["registers"][15].as_u64().expect("PC") as u32;
        session
            .memory(pc, 4)
            .await
            .expect("mapped instruction memory");
        assert!(session.memory(pc, 4097).await.is_err(), "bounded read");
    }
    session.pause().await.expect("pause outside nested loop");
    let current = session.capture().await.expect("capture after pause");
    session.reset().await.expect("new epoch");
    assert!(
        session.read_lease(&current[0].lease).is_err(),
        "reset invalidates leases"
    );
    session.advance(2).await.expect("run after reset");
    let before_load = session.capture().await.expect("frame before reload");
    session.load(&fixture).await.expect("reload content");
    assert!(
        session.read_lease(&before_load[0].lease).is_err(),
        "load invalidates previous content leases"
    );
    session.stop().await.expect("clean shutdown");
    let mut second = NativeSession::spawn(&executable, &root)
        .await
        .expect("root lock released");
    second.load(&fixture).await.expect("second launch");
    second.advance(2).await.expect("second run");
    second.stop().await.expect("second shutdown");
    std::fs::remove_dir_all(root).expect("all instance files released");
}

#[tokio::test]
#[ignore = "requires explicitly supplied native executable and homebrew fixture"]
async fn four_native_workers_survive_forced_sibling_failure() {
    let (executable, fixture, root) = config();
    let mut tasks = Vec::new();
    for index in 0..4 {
        let executable = executable.clone();
        let fixture = fixture.clone();
        let root = root.join(index.to_string());
        tasks.push(tokio::spawn(async move {
            let mut session = NativeSession::spawn(&executable, &root)
                .await
                .expect("isolated native child");
            session
                .load(&fixture)
                .await
                .expect("same read-only content");
            session.advance(30).await.expect("parallel emulation");
            let frames = session.capture().await.expect("private frame artifacts");
            if index == 0 {
                session.run().await.expect("run during failure injection");
                session.abort().await;
                assert!(session.read_lease(&frames[0].lease).is_err());
                assert!(session.status().await.is_err());
            } else {
                session.advance(30).await.expect("surviving sibling");
                session.stop().await.expect("clean sibling stop");
            }
            root
        }));
    }
    for task in tasks {
        let child_root = task.await.expect("worker test task");
        assert!(child_root.join("worker.stderr.log").exists());
    }
    std::fs::remove_dir_all(root).expect("job handles and locks released after crash");
}

#[tokio::test]
#[ignore = "requires explicitly supplied native executable and homebrew fixture"]
async fn supervisor_bounds_workers_and_retains_scoped_artifacts() {
    let (worker, fixture, root) = config();
    let mut host = Supervisor::new(HostConfig {
        worker,
        storage_root: root.clone(),
        assets: std::collections::BTreeMap::from([("fixture".into(), fixture)]),
        allow_control: true,
    })
    .expect("native host");
    assert!(host.create("../unapproved.3dsx").await.is_err());
    let mut sessions = Vec::new();
    for _ in 0..4 {
        sessions.push(host.create("fixture").await.expect("within worker budget"));
    }
    assert!(
        host.create("fixture").await.is_err(),
        "fifth worker must be rejected before launch"
    );
    host.session(&sessions[0])
        .expect("owned session")
        .advance(2)
        .await
        .expect("native frames");
    let capture = host
        .capture(&sessions[0])
        .await
        .expect("immutable artifacts");
    for id in sessions {
        host.stop(&id).await.expect("stop owned worker");
    }
    assert!(host.read_artifact("file:///C:/Windows/win.ini").is_err());
    for artifact in capture {
        assert_eq!(
            host.read_artifact(&artifact.uri)
                .expect("artifact survives worker stop")
                .len(),
            artifact.bytes
        );
    }
    std::fs::remove_dir_all(root).expect("host releases all worker roots");
}
