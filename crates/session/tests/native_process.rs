//! Opt-in native acceptance. Assets remain outside Git; no network or ROM download.
use std::path::PathBuf;

use lotus_protocol::envelope::SessionUuid;
use lotus_session::native_session::{InputState, NativeSession};

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
