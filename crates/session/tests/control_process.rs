//! Real child-process acceptance, not an in-memory transport simulation.
use std::time::Duration;

use lotus_session::controlled_worker::ControlledWorker;
use lotus_session::{SessionState, WorkerSpec};

fn spec(mode: &str) -> WorkerSpec {
    let id = lotus_protocol::envelope::SessionUuid::generate();
    WorkerSpec::new(
        env!("CARGO_BIN_EXE_lotus-mock-worker"),
        std::env::temp_dir().join(format!("lotus-control-{id}")),
    )
    .arg(mode)
}

#[tokio::test]
async fn handshake_query_clean_shutdown_and_second_launch() {
    for _ in 0..2 {
        let spec = spec("");
        let mut worker = ControlledWorker::spawn(&spec, Duration::from_secs(3))
            .await
            .expect("mock handshake");
        let reply = worker.status().await.expect("status over pipes");
        assert_eq!(reply.backend, "mock");
        worker.stop().await.expect("graceful exit");
        assert_eq!(worker.state(), SessionState::Stopped);
        assert!(
            worker.status().await.is_err(),
            "stopped connection cannot be reused"
        );
        std::fs::remove_dir_all(spec.storage_root).expect("remove test storage");
    }
}

#[tokio::test]
async fn failed_handshakes_are_reaped_and_logs_retained() {
    for mode in ["crash", "stale", "hang"] {
        let spec = spec(mode);
        assert!(
            ControlledWorker::spawn(&spec, Duration::from_millis(500))
                .await
                .is_err(),
            "{mode} must fail"
        );
        assert!(spec.storage_root.join("worker.stderr.log").is_file());
        std::fs::remove_dir_all(spec.storage_root).expect("child has released its files");
    }
}

#[tokio::test]
async fn four_private_connections_survive_a_sibling_failure() {
    let mut workers = Vec::new();
    let mut specs = Vec::new();
    for _ in 0..4 {
        let spec = spec("");
        workers.push(
            ControlledWorker::spawn(&spec, Duration::from_secs(3))
                .await
                .expect("spawn isolated child"),
        );
        specs.push(spec);
    }
    assert!(
        ControlledWorker::spawn(&spec("crash"), Duration::from_secs(3))
            .await
            .is_err()
    );
    for (mut worker, spec) in workers.into_iter().zip(specs) {
        worker.status().await.expect("sibling remains responsive");
        worker.stop().await.expect("sibling exits");
        std::fs::remove_dir_all(spec.storage_root).expect("remove test storage");
    }
}

#[tokio::test]
async fn running_child_crash_transitions_to_crashed() {
    let spec = spec("crash_status");
    let mut worker = ControlledWorker::spawn(&spec, Duration::from_secs(3))
        .await
        .expect("handshake before crash");
    assert!(worker.status().await.is_err());
    assert_eq!(worker.state(), SessionState::Crashed);
    assert!(
        worker.status().await.is_err(),
        "failed commands cannot be retried"
    );
    std::fs::remove_dir_all(spec.storage_root).expect("reaped child releases log");
}

#[cfg(windows)]
#[tokio::test]
async fn abort_terminates_descendants_and_releases_root_lock() {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading;
    let spec = spec("spawn_descendant");
    let mut worker = ControlledWorker::spawn(&spec, Duration::from_secs(3))
        .await
        .expect("contained parent");
    assert!(ControlledWorker::spawn(&spec, Duration::from_secs(3))
        .await
        .is_err());
    let pid: u32 = std::fs::read_to_string(spec.storage_root.join("descendant.pid"))
        .expect("descendant PID")
        .parse()
        .expect("PID number");
    // SAFETY: OpenProcess returns an owned handle; waiting observes only this test child.
    let descendant = unsafe {
        let raw = Threading::OpenProcess(Threading::PROCESS_SYNCHRONIZE, 0, pid);
        assert!(!raw.is_null());
        OwnedHandle::from_raw_handle(raw)
    };
    worker.abort().await;
    // SAFETY: the owned process handle stays live for the wait.
    assert_eq!(
        unsafe { Threading::WaitForSingleObject(descendant.as_raw_handle(), 3000) },
        0
    );
    let mut replacement = ControlledWorker::spawn(
        &WorkerSpec::new(&spec.executable, &spec.storage_root),
        Duration::from_secs(3),
    )
    .await
    .expect("lock released after abort");
    replacement.stop().await.expect("replacement exits");
    std::fs::remove_dir_all(spec.storage_root).expect("files released");
}
