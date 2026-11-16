//! Opt-in native acceptance. Assets remain outside Git; no network or ROM download.
use std::path::PathBuf;

use lotus_protocol::envelope::SessionUuid;
use lotus_session::native_session::{InputState, NativeSession};
use lotus_session::supervisor::{HostConfig, SessionKind, Supervisor};
use lotus_session::telemetry::Actor;

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

/// Issue #6: live previews come from each worker's shared-memory ring, match an explicit
/// capture whenever the guest is paused, never carry an old epoch, and stay separate between
/// concurrent workers and across a worker restart.
#[tokio::test]
#[ignore = "requires explicitly supplied native executable and homebrew fixture"]
async fn shared_previews_match_capture_per_worker_and_epoch() {
    let (executable, fixture, root) = config();
    let other_root = root.with_extension("other");
    let mut a = NativeSession::spawn(&executable, &root).await.expect("a");
    let mut b = NativeSession::spawn(&executable, &other_root)
        .await
        .expect("b");
    for session in [&mut a, &mut b] {
        session.load(&fixture).await.expect("load");
    }
    a.advance(30).await.expect("a runs");
    b.advance(12).await.expect("b runs");
    let pixels = |frames: &[lotus_session::native_session::Frame]| {
        frames.iter().map(|f| f.bgra.clone()).collect::<Vec<_>>()
    };
    let (preview_a, frame_a) = a.preview().await.expect("a preview");
    let (preview_b, frame_b) = b.preview().await.expect("b preview");
    assert_eq!(frame_a, Some(30), "paused preview is the current frame");
    assert_eq!(frame_b, Some(12));
    assert!(
        preview_a.iter().all(|f| f.lease.path.is_empty()),
        "shared memory, not files"
    );
    assert_eq!(pixels(&preview_a), pixels(&a.capture().await.unwrap()));
    assert_eq!(pixels(&preview_b), pixels(&b.capture().await.unwrap()));
    a.run().await.expect("run");
    for _ in 0..20 {
        let (frames, shown) = a.preview().await.expect("running preview");
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|f| f.lease.epoch == a.epoch()));
        assert!(shown.is_some());
    }
    a.pause().await.expect("pause");
    let old_epoch = a.epoch();
    a.reset().await.expect("reset");
    a.advance(2).await.expect("run after reset");
    let (after_reset, _) = a.preview().await.expect("preview after reset");
    assert!(a.epoch() != old_epoch);
    assert!(after_reset.iter().all(|f| f.lease.epoch == a.epoch()));
    assert_eq!(pixels(&after_reset), pixels(&a.capture().await.unwrap()));
    a.stop().await.expect("stop a");
    let mut restarted = NativeSession::spawn(&executable, &root)
        .await
        .expect("restart");
    restarted.load(&fixture).await.expect("reload");
    restarted.advance(5).await.expect("run");
    let (fresh, shown) = restarted.preview().await.expect("restarted preview");
    assert_eq!(shown, Some(5));
    assert_eq!(pixels(&fresh), pixels(&restarted.capture().await.unwrap()));
    restarted.stop().await.expect("stop restarted");
    b.stop().await.expect("stop b");
    std::fs::remove_dir_all(root).expect("release a");
    std::fs::remove_dir_all(other_root).expect("release b");
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
async fn supervisor_bounds_concurrent_sessions_and_retains_scoped_artifacts() {
    let (worker, fixture, root) = config();
    let mut config = HostConfig::new(worker, root.clone());
    config.assets = std::collections::BTreeMap::from([("fixture".into(), fixture)]);
    config.allow_control = true;
    let host = Supervisor::new(config).expect("native host");
    let primary = |card: &str| SessionKind::Primary { card: card.into() };
    let secondary = |parent: Option<&str>| SessionKind::Secondary {
        parent: parent.map(str::to_string),
    };
    assert!(host
        .create(secondary(None), Some("../unapproved.3dsx"), Actor::Host)
        .await
        .is_err());
    // Two primaries and two secondaries start concurrently against one limit.
    let (a, b, c, d) = tokio::join!(
        host.create(primary("one"), Some("fixture"), Actor::Host),
        host.create(primary("two"), Some("fixture"), Actor::Host),
        host.create(secondary(Some("one")), Some("fixture"), Actor::Host),
        host.create(secondary(None), None, Actor::Host),
    );
    let sessions: Vec<String> = [a, b, c, d]
        .into_iter()
        .map(|created| created.expect("within session budget").id)
        .collect();
    assert!(
        host.create(secondary(None), None, Actor::Host)
            .await
            .is_err(),
        "fifth session must be rejected before launch"
    );
    // Independent locks: two sessions advance at the same time.
    let (first, second) = tokio::join!(
        host.advance(&sessions[0], 2, Actor::Host),
        host.advance(&sessions[2], 2, Actor::Host),
    );
    first.expect("primary frames");
    second.expect("secondary frames");
    let capture = host
        .capture(&sessions[0])
        .await
        .expect("immutable artifacts");
    for id in &sessions {
        host.stop(id, Actor::Host).await.expect("stop owned worker");
    }
    assert!(root.join("cards/one").is_dir() && root.join("cards/two").is_dir());
    for id in &sessions[2..] {
        assert!(
            !root.join("scratch").join(id).exists(),
            "secondary discarded"
        );
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
