//! Supervisor responsiveness isolation against real child processes (`lotus-mock-native`).
//!
//! The mock answers like `ctr_worker/control.h` and stalls one reply when its root holds
//! `mock-stall-ms`, which is how these tests hang one worker while its siblings keep going.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lotus_protocol::envelope::SessionUuid;
use lotus_session::native_session::InputState;
use lotus_session::supervisor::{HostConfig, SessionKind, Supervisor};
use lotus_session::telemetry::Actor;

struct Host {
    base: PathBuf,
    host: Arc<Supervisor>,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn host(name: &str, sessions: usize) -> Host {
    let base = std::env::temp_dir().join(format!("lotus-sup-{name}-{}", SessionUuid::generate()));
    std::fs::create_dir_all(&base).expect("base");
    let content = base.join("demo.3dsx");
    std::fs::write(&content, b"3DSX").expect("content");
    let mut config = HostConfig::new(env!("CARGO_BIN_EXE_lotus-mock-native"), base.join("store"));
    config.allow_control = true;
    config.max_sessions = sessions;
    config.assets.insert("demo".into(), content);
    Host {
        host: Arc::new(Supervisor::new(config).expect("host")),
        base,
    }
}

impl Host {
    async fn running(&self, count: usize) -> Vec<String> {
        let mut ids = Vec::new();
        for _ in 0..count {
            let info = self
                .host
                .create(
                    SessionKind::Secondary { parent: None },
                    Some("demo"),
                    Actor::Host,
                )
                .await
                .expect("create");
            self.host.run(&info.id, Actor::Host).await.expect("run");
            ids.push(info.id);
        }
        ids
    }

    fn root(&self, id: &str) -> PathBuf {
        self.base
            .join("store")
            .canonicalize()
            .expect("store")
            .join("scratch")
            .join(id)
    }

    fn stall(&self, id: &str, ms: u64) {
        std::fs::write(self.root(id).join("mock-stall-ms"), ms.to_string()).expect("stall");
    }
}

fn telemetry(host: &Supervisor, id: &str) -> lotus_session::telemetry::Telemetry {
    host.summary(id).expect("summary").telemetry
}

fn inputs(root: &Path) -> Vec<u64> {
    std::fs::read_to_string(root.join("inputs.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).expect("json")["buttons"]
                .as_u64()
                .expect("buttons")
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stalled_worker_delays_only_itself() {
    let fixture = host("stall", 3);
    let ids = fixture.running(3).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    fixture.stall(&ids[0], 3000);
    // Let the stalled worker's next status exchange begin.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before: Vec<_> = ids.iter().map(|id| telemetry(&fixture.host, id)).collect();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let after: Vec<_> = ids.iter().map(|id| telemetry(&fixture.host, id)).collect();
    assert_eq!(
        after[0].supervision.polls, before[0].supervision.polls,
        "stalled worker completed no status exchange"
    );
    for sibling in 1..3 {
        let polls = after[sibling].supervision.polls - before[sibling].supervision.polls;
        // 100 ms cadence over 1.5 s; a global barrier would give zero here.
        assert!(polls >= 8, "sibling {sibling} polled {polls} times");
        assert!(
            after[sibling].supervision.continuations > before[sibling].supervision.continuations,
            "sibling {sibling} bounded runs were continued (600-frame batches end every second)"
        );
        assert!(after[sibling].playing && after[sibling].frames > before[sibling].frames);
    }
    // The stalled worker recovers on its own and is reported as late, not failed.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let recovered = telemetry(&fixture.host, &ids[0]);
    assert!(recovered.supervision.polls > after[0].supervision.polls);
    assert!(recovered.supervision.status_max_ms >= 2500.0);
    assert_eq!(recovered.supervision.status_failures, 0);
    assert!(recovered.playing);
    fixture.host.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn back_to_back_commands_do_not_starve_supervision() {
    let fixture = host("starve", 1);
    let id = fixture.running(1).await.remove(0);
    let start = Instant::now();
    let mut commands = 0;
    // The mock's 600-frame runs last one second each.
    while start.elapsed() < Duration::from_millis(2500) {
        fixture.host.registers(&id, 0).await.expect("registers");
        commands += 1;
    }
    let status = telemetry(&fixture.host, &id);
    assert!(commands > 100, "{commands} commands");
    // A poll queues behind at most the command in flight after MAX_BUSY_POLLS skips.
    assert!(status.supervision.polls >= 3, "{:?}", status.supervision);
    assert!(
        status.supervision.continuations >= 1,
        "{:?}",
        status.supervision
    );
    assert!(status.playing && status.frames > 600, "{}", status.frames);
    fixture.host.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_manual_pause_is_never_resumed() {
    let fixture = host("pause", 1);
    let id = fixture.running(1).await.remove(0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    fixture.host.pause(&id, Actor::Host).await.expect("pause");
    // Gauges refresh at the next poll after the pause.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let paused = telemetry(&fixture.host, &id);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let later = telemetry(&fixture.host, &id);
    assert!(!later.playing);
    assert_eq!(
        later.supervision.continuations,
        paused.supervision.continuations
    );
    assert_eq!(later.frames, paused.frames);
    assert!(later.supervision.polls > paused.supervision.polls);
    fixture.host.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_to_one_worker_ignores_a_sibling_stall() {
    let fixture = host("input", 4);
    let ids = fixture.running(4).await;
    fixture.stall(&ids[0], 3000);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tap = |buttons| InputState {
        buttons,
        ..InputState::default()
    };
    let started = Instant::now();
    for round in 0..20 {
        for id in &ids {
            // Rapid taps: press and release within one submission burst.
            for buttons in [1, 0] {
                fixture
                    .host
                    .submit_input(id, tap(buttons), "live", Actor::Desktop)
                    .expect("submit never waits");
            }
        }
        if round == 0 {
            assert!(started.elapsed() < Duration::from_millis(100));
        }
        tokio::time::sleep(Duration::from_millis(16)).await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    for (index, id) in ids.iter().enumerate().skip(1) {
        let input = telemetry(&fixture.host, id).input;
        assert!(
            input.latency_p95_ms < 500.0,
            "worker {index} p95 {} ms during a sibling stall",
            input.latency_p95_ms
        );
        let seen = inputs(&fixture.root(id));
        assert_eq!(seen.last(), Some(&0), "no stuck key on worker {index}");
        let presses = seen.windows(2).filter(|w| w[0] == 0 && w[1] == 1).count()
            + usize::from(seen.first() == Some(&1));
        assert_eq!(presses, 20, "every tap reached worker {index}: {seen:?}");
    }
    // The stalled worker gets its queue afterwards, ending released.
    tokio::time::sleep(Duration::from_millis(3000)).await;
    let stalled = telemetry(&fixture.host, &ids[0]).input;
    assert!(stalled.delivered > 0 && stalled.pending == 0);
    assert_eq!(inputs(&fixture.root(&ids[0])).last(), Some(&0));
    fixture.host.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reset_cancels_pending_input_and_stop_ends_tasks() {
    let fixture = host("cancel", 1);
    let id = fixture.running(1).await.remove(0);
    fixture.stall(&id, 800);
    tokio::time::sleep(Duration::from_millis(200)).await;
    for buttons in [1, 0, 1] {
        fixture
            .host
            .submit_input(
                &id,
                InputState {
                    buttons,
                    ..InputState::default()
                },
                "live",
                Actor::Desktop,
            )
            .expect("submit");
    }
    fixture.host.reset(&id, Actor::Host).await.expect("reset");
    let input = telemetry(&fixture.host, &id).input;
    assert!(input.cancelled >= 1, "{input:?}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(inputs(&fixture.root(&id)).len() <= 1);
    fixture.host.stop(&id, Actor::Host).await.expect("stop");
    assert!(fixture.host.summary(&id).is_err());
    assert!(fixture
        .host
        .submit_input(&id, InputState::default(), "live", Actor::Desktop)
        .is_err());
}

fn process_alive(pid: u32) -> bool {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
}

#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn close_is_bounded_by_one_deadline_with_hung_workers() {
    let fixture = host("close", 16);
    let ids = fixture.running(16).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pids: Vec<u32> = ids
        .iter()
        .map(|id| telemetry(&fixture.host, id).pid.expect("pid"))
        .collect();
    // Half the workers hang their next reply for a minute: a status exchange holds each.
    for id in ids.iter().step_by(2) {
        fixture.stall(id, 60_000);
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    let started = Instant::now();
    let outcomes = fixture.host.close().await;
    let elapsed = started.elapsed();
    eprintln!("close of 16 workers, 8 hung: {elapsed:?}");
    // Sequential stops would take 8 x 3 s; concurrent ones one deadline plus termination.
    assert!(elapsed < Duration::from_secs(7), "close took {elapsed:?}");
    assert_eq!(outcomes.len(), 16);
    for (index, id) in ids.iter().enumerate() {
        let (_, result) = outcomes
            .iter()
            .find(|(outcome, _)| outcome == id)
            .expect("every session reported");
        let root = fixture.root(id);
        if index % 2 == 0 {
            let error = result.as_ref().expect_err("hung worker is forced");
            assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
            // Abnormal end keeps the logs only.
            assert!(root.join("worker.stderr.log").is_file());
            assert!(!root.join("worker.lock").exists());
        } else {
            result.as_ref().expect("healthy worker stops cleanly");
            assert!(!root.exists(), "clean secondary root removed");
        }
    }
    for pid in pids {
        assert!(!process_alive(pid), "worker {pid} survived close");
    }
    assert!(fixture.host.sessions().is_empty());
    let refused = fixture
        .host
        .create(SessionKind::Secondary { parent: None }, None, Actor::Host)
        .await;
    assert!(
        refused.is_err(),
        "no new sessions while the host is closing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_stop_of_one_session_is_refused_not_doubled() {
    let fixture = host("double", 1);
    let id = fixture.running(1).await.remove(0);
    let (first, second) = tokio::join!(
        fixture.host.stop(&id, Actor::Host),
        fixture.host.stop(&id, Actor::Host)
    );
    assert!(first.is_ok() != second.is_ok(), "{first:?} {second:?}");
    assert!(fixture.host.sessions().is_empty());
}
