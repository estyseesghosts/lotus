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
    host_with(name, sessions, |_| {})
}

fn host_with(name: &str, sessions: usize, configure: impl FnOnce(&mut HostConfig)) -> Host {
    let base = std::env::temp_dir().join(format!("lotus-sup-{name}-{}", SessionUuid::generate()));
    std::fs::create_dir_all(&base).expect("base");
    let content = base.join("demo.3dsx");
    std::fs::write(&content, b"3DSX").expect("content");
    let mut config = HostConfig::new(env!("CARGO_BIN_EXE_lotus-mock-native"), base.join("store"));
    config.allow_control = true;
    config.max_sessions = sessions;
    config.assets.insert("demo".into(), content);
    configure(&mut config);
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

/// lotus#44: concurrent enables on different sessions may not exceed the rewind budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_rewind_enables_respect_the_budget() {
    use lotus_protocol::debug::DebugCommand;
    let base = std::env::temp_dir().join(format!("lotus-sup-budget-{}", SessionUuid::generate()));
    std::fs::create_dir_all(&base).expect("base");
    let content = base.join("demo.3dsx");
    std::fs::write(&content, b"3DSX").expect("content");
    let mut config = HostConfig::new(env!("CARGO_BIN_EXE_lotus-mock-native"), base.join("store"));
    config.allow_control = true;
    config.max_sessions = 4;
    config.assets.insert("demo".into(), content);
    config.policy.rewind_sessions = Some(1);
    let fixture = Host {
        host: Arc::new(Supervisor::new(config).expect("host")),
        base,
    };
    let ids = fixture.running(3).await;
    let enable = DebugCommand::RewindConfigure {
        enabled: true,
        capacity: 8,
        budget_mib: 64,
        interval_frames: None,
        compression_level: None,
    };
    for round in 0..3 {
        // Every worker's next reply stalls, so all requests are in flight together.
        for id in &ids {
            fixture.stall(id, 400);
        }
        let results = futures_join(&fixture.host, &ids, &enable).await;
        let admitted = results.iter().filter(|result| result.is_ok()).count();
        assert_eq!(admitted, 1, "round {round}: {results:?}");
        let status = fixture.host.policy_status();
        assert_eq!(status["rewind"]["sessions"].as_array().unwrap().len(), 1);
        assert!(status["rewind"]["pending"].as_object().unwrap().is_empty());
        // Disable everywhere; the next round starts from an empty budget.
        let holder = &ids[results.iter().position(Result::is_ok).unwrap()];
        let disable = DebugCommand::RewindConfigure {
            enabled: false,
            capacity: 8,
            budget_mib: 64,
            interval_frames: None,
            compression_level: None,
        };
        fixture
            .host
            .debug(holder, &disable, Actor::Host)
            .await
            .expect("disable");
    }
    // A worker that rejects the enable leaves no holder and no pending slot.
    std::fs::write(fixture.root(&ids[0]).join("mock-reject-debug"), b"").expect("reject");
    assert!(fixture
        .host
        .debug(&ids[0], &enable, Actor::Host)
        .await
        .is_err());
    let status = fixture.host.policy_status();
    assert!(status["rewind"]["sessions"].as_array().unwrap().is_empty());
    fixture
        .host
        .debug(&ids[1], &enable, Actor::Host)
        .await
        .expect("budget was not leaked");
}

async fn futures_join(
    host: &Arc<Supervisor>,
    ids: &[String],
    command: &lotus_protocol::debug::DebugCommand,
) -> Vec<std::io::Result<serde_json::Value>> {
    let tasks: Vec<_> = ids
        .iter()
        .map(|id| {
            let host = host.clone();
            let id = id.clone();
            let command = command.clone();
            tokio::spawn(async move { host.debug(&id, &command, Actor::Host).await })
        })
        .collect();
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.expect("task"));
    }
    results
}

/// lotus#48: a new owner's state equal to the in-flight one is still delivered, after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_change_during_a_stalled_input_is_delivered() {
    let fixture = host("owner", 1);
    let ids = fixture.running(1).await;
    let id = &ids[0];
    let root = fixture.root(id);
    let pressed = InputState {
        buttons: 1,
        ..InputState::default()
    };
    // Wait out any status exchange already started, then stall the next reply.
    tokio::time::sleep(Duration::from_millis(300)).await;
    fixture.stall(id, 800);
    fixture
        .host
        .submit_input(id, pressed, "live", Actor::Desktop)
        .expect("live");
    tokio::time::sleep(Duration::from_millis(200)).await;
    fixture
        .host
        .submit_input(id, pressed, "scenario", Actor::Mcp)
        .expect("scenario");
    let deadline = Instant::now() + Duration::from_secs(5);
    let owners = loop {
        let owners: Vec<String> = std::fs::read_to_string(root.join("inputs.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).expect("json")["owner"]
                    .as_str()
                    .expect("owner")
                    .to_string()
            })
            .collect();
        if owners.len() >= 2 || Instant::now() > deadline {
            break owners;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(owners, vec!["live".to_string(), "scenario".to_string()]);
    assert_eq!(inputs(&root), vec![1, 1]);
    assert_eq!(telemetry(&fixture.host, id).input.owner_handoffs, 1);
}

/// lotus#47: a worker that logs a critical line and exits is reported with that line and a
/// cause, while a sibling keeps running.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crashed_worker_reports_its_logged_cause() {
    let fixture = host("crash", 3);
    let ids = fixture.running(3).await;
    let cases: [(&[u8], &str); 3] = [
        (
            b"[ 305.918224] Render.Vulkan <Critical> video_core/renderer_vulkan/vk_texture_runtime.cpp:Create:224: Failed allocating image with error -2\n",
            "vulkan_out_of_memory",
        ),
        (b"\xff\xfe invalid utf-8 only\n", "abort"),
        (b"", "abort"),
    ];
    for ((log, _), id) in cases.iter().zip(&ids) {
        let root = fixture.root(id);
        std::fs::create_dir_all(root.join("log")).expect("log dir");
        if !log.is_empty() {
            std::fs::write(root.join("log/azahar_log.txt"), log).expect("log");
        }
    }
    // Crash the first two; the third keeps running until its turn.
    for id in &ids[..2] {
        std::fs::write(fixture.root(id).join("mock-exit-code"), "3").expect("exit");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while ids[..2]
        .iter()
        .any(|id| telemetry(&fixture.host, id).failure.is_none())
    {
        assert!(Instant::now() < deadline, "failures not reported");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let sibling = telemetry(&fixture.host, &ids[2]);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        telemetry(&fixture.host, &ids[2]).frames > sibling.frames,
        "sibling progresses"
    );
    for ((_, cause), id) in cases.iter().zip(&ids).take(2) {
        let failure = telemetry(&fixture.host, id).failure.expect("failure");
        assert_eq!(failure["cause"], *cause, "{failure}");
        assert_eq!(failure["exit_code"], 3);
        let summary = failure["summary"].as_str().unwrap();
        assert!(!summary.contains(":\\"), "path leaked: {summary}");
        assert!(fixture.root(id).join("failure.json").is_file());
    }
    let first = telemetry(&fixture.host, &ids[0]);
    assert!(
        first
            .last_error
            .contains("Failed allocating image with error -2"),
        "{}",
        first.last_error
    );
    assert!(first.failure.unwrap()["renderer"] == true);
    let report = fixture
        .host
        .failure_report(&ids[0])
        .await
        .expect("report")
        .expect("some");
    assert!(report.contains("log_tail"));
    // No log, killed from outside: an abort by exit code only, still path-free.
    std::fs::write(fixture.root(&ids[2]).join("mock-exit-code"), "1").expect("exit");
    let deadline = Instant::now() + Duration::from_secs(5);
    while telemetry(&fixture.host, &ids[2]).failure.is_none() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        telemetry(&fixture.host, &ids[2]).failure.unwrap()["cause"],
        "abort"
    );
}

/// `status.gpu_memory` of one Vulkan session: one 16 GiB device-local heap with
/// `VK_EXT_memory_budget`, `headroom_mib` left and a `high_water_mib` peak.
fn gpu_memory(headroom_mib: u64, high_water_mib: u64) -> String {
    let usage = 2000u64 << 20;
    serde_json::json!({"budget_source": "VK_EXT_memory_budget", "allocation_failures": 0,
        "heaps": [{"device_local": true, "size": 16u64 << 30, "budget": usage + (headroom_mib << 20),
            "usage": usage, "block_bytes": high_water_mib << 20,
            "block_bytes_high_water": high_water_mib << 20, "allocation_bytes": 0}]})
    .to_string()
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn sticks(root: &Path) -> Vec<(u64, serde_json::Value, serde_json::Value)> {
    std::fs::read_to_string(root.join("inputs.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("json");
            (
                value["buttons"].as_u64().expect("buttons"),
                value["circle_pad"].clone(),
                value["c_stick"].clone(),
            )
        })
        .collect()
}

/// lotus#60: stick positions travel each session's own mailbox next to button edges, bad
/// ranges are refused before queueing, and a worker without `analog_input` refuses a moved
/// stick instead of dropping it while neutral input still works.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn analog_input_is_per_session_and_refused_by_old_workers() {
    let fixture = host("analog", 3);
    let ids = fixture.running(2).await;
    for step in 1..=10u32 {
        for (index, id) in ids.iter().enumerate() {
            let sign = if index == 0 { 1.0 } else { -1.0 };
            let state = InputState {
                buttons: step % 2,
                circle_pad: [sign * step as f32 / 10.0, 0.0],
                c_stick: [0.0, sign * step as f32 / 10.0],
                ..InputState::default()
            };
            fixture
                .host
                .submit_input(id, state, "live", Actor::Desktop)
                .expect("submit");
        }
    }
    for id in &ids {
        fixture
            .host
            .submit_input(id, InputState::default(), "live", Actor::Desktop)
            .expect("neutral");
    }
    until("both sessions end neutral", || {
        ids.iter().all(|id| {
            sticks(&fixture.root(id))
                .last()
                .is_some_and(|(buttons, circle, cstick)| {
                    *buttons == 0
                        && *circle == serde_json::json!([0.0, 0.0])
                        && *cstick == serde_json::json!([0.0, 0.0])
                })
        })
    })
    .await;
    for (index, id) in ids.iter().enumerate() {
        let sign = if index == 0 { 1.0 } else { -1.0 };
        let delivered = sticks(&fixture.root(id));
        // Never the sibling's direction; at least one off-centre sample got through.
        assert!(delivered.iter().all(|(_, circle, cstick)| {
            circle[0].as_f64().expect("x") * sign >= 0.0
                && cstick[1].as_f64().expect("y") * sign >= 0.0
        }));
        assert!(delivered.iter().any(|(_, circle, _)| circle[0] != 0.0));
        // Button edges are not lost to stick coalescing: presses and releases alternate.
        assert!(delivered
            .windows(2)
            .any(|pair| pair[0].0 == 1 && pair[1].0 == 0));
    }
    for axis in [1.5, f32::NAN, f32::INFINITY] {
        let error = fixture
            .host
            .submit_input(
                &ids[0],
                InputState {
                    circle_pad: [axis, 0.0],
                    ..InputState::default()
                },
                "live",
                Actor::Desktop,
            )
            .expect_err("out of range");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{error}");
    }

    let card = fixture
        .base
        .join("store")
        .canonicalize()
        .expect("store")
        .join("cards/old");
    std::fs::create_dir_all(&card).expect("card");
    std::fs::write(card.join("mock-no-flush"), "").expect("old worker");
    let old = fixture
        .host
        .create(
            SessionKind::Primary { card: "old".into() },
            Some("demo"),
            Actor::Host,
        )
        .await
        .expect("old worker")
        .id;
    let moved = InputState {
        c_stick: [0.0, 1.0],
        ..InputState::default()
    };
    for error in [
        fixture
            .host
            .submit_input(&old, moved, "live", Actor::Desktop)
            .expect_err("queued"),
        fixture
            .host
            .input(&old, moved, "live", Actor::Host)
            .await
            .expect_err("direct"),
    ] {
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported, "{error}");
    }
    fixture
        .host
        .input(
            &old,
            InputState {
                buttons: 1,
                ..InputState::default()
            },
            "live",
            Actor::Host,
        )
        .await
        .expect("buttons still work");
    let sent = sticks(&card);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (sent[0].1.clone(), sent[0].2.clone()),
        (serde_json::Value::Null, serde_json::Value::Null)
    );
    fixture.host.close().await;
}

/// lotus#46: with too little device-local headroom for the largest running session's peak, a
/// session that only prefers Vulkan starts on software with the reason, and an explicit
/// Vulkan profile is refused with the numbers. Running sessions are untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gpu_pressure_starts_preferred_vulkan_on_software_and_refuses_explicit() {
    let fixture = host("gpu-admit", 3);
    let ids = fixture.running(2).await;
    until("renderer", || {
        !telemetry(&fixture.host, &ids[0]).renderer.is_empty()
    })
    .await;
    assert_eq!(telemetry(&fixture.host, &ids[0]).renderer, "vulkan");
    for id in &ids {
        std::fs::write(
            fixture.root(id).join("mock-gpu-memory"),
            gpu_memory(500, 900),
        )
        .expect("gpu");
    }
    until("gpu telemetry", || {
        ids.iter()
            .all(|id| telemetry(&fixture.host, id).gpu_memory.is_some())
    })
    .await;
    let policy = fixture.host.policy_status();
    assert_eq!(policy["gpu_memory"]["headroom_mib"], 500, "{policy}");
    assert_eq!(policy["gpu_memory"]["estimate_mib"], 900);
    assert_eq!(policy["gpu_memory"]["pressure"], true);
    let third = fixture
        .host
        .create(
            SessionKind::Secondary { parent: None },
            Some("demo"),
            Actor::Host,
        )
        .await
        .expect("software session");
    until("renderer", || {
        !telemetry(&fixture.host, &third.id).renderer.is_empty()
    })
    .await;
    let third = telemetry(&fixture.host, &third.id);
    assert_eq!(third.renderer, "software");
    assert!(
        third
            .renderer_fallback
            .contains("500 MiB device-local headroom"),
        "{}",
        third.renderer_fallback
    );

    let explicit = host_with("gpu-refuse", 3, |config| {
        config.profile.insert(
            "graphics_api".into(),
            lotus_protocol::debug::SettingValue::Integer(2),
        );
    });
    let ids = explicit.running(2).await;
    std::fs::write(
        explicit.root(&ids[0]).join("mock-gpu-memory"),
        gpu_memory(500, 900),
    )
    .expect("gpu");
    until("gpu telemetry", || {
        telemetry(&explicit.host, &ids[0]).gpu_memory.is_some()
    })
    .await;
    let error = explicit
        .host
        .create(
            SessionKind::Secondary { parent: None },
            Some("demo"),
            Actor::Host,
        )
        .await
        .expect_err("refused");
    assert_eq!(error.kind(), std::io::ErrorKind::OutOfMemory);
    assert!(
        error.to_string().contains("reached up to 900 MiB"),
        "{error}"
    );
    let before = telemetry(&explicit.host, &ids[1]).frames;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        telemetry(&explicit.host, &ids[1]).frames > before,
        "sibling progresses"
    );
}

/// lotus#46: a worker that reports `worker_gpu_error` and exits 4 fails with the typed cause
/// and the report, while its sibling keeps running.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_worker_gpu_error_is_typed_and_isolated() {
    let fixture = host("gpu-error", 2);
    let ids = fixture.running(2).await;
    let report = serde_json::json!({"cause": "gpu_out_of_memory", "vk_result": -2,
        "vk_result_name": "VK_ERROR_OUT_OF_DEVICE_MEMORY", "object": "image", "frame": 40,
        "gpu_memory": serde_json::from_str::<serde_json::Value>(&gpu_memory(0, 900)).unwrap()});
    std::fs::write(
        fixture.root(&ids[0]).join("mock-gpu-error"),
        report.to_string(),
    )
    .expect("error");
    until("failure", || {
        telemetry(&fixture.host, &ids[0]).failure.is_some()
    })
    .await;
    let failure = telemetry(&fixture.host, &ids[0]).failure.unwrap();
    assert_eq!(failure["cause"], "vulkan_out_of_memory", "{failure}");
    assert_eq!(failure["exit_code"], 4);
    assert_eq!(failure["renderer"], true);
    assert_eq!(failure["gpu"]["object"], "image");
    assert_eq!(failure["gpu"]["frame"], 40);
    assert!(failure["summary"]
        .as_str()
        .unwrap()
        .contains("VK_ERROR_OUT_OF_DEVICE_MEMORY (-2) creating image"));
    let before = telemetry(&fixture.host, &ids[1]).frames;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        telemetry(&fixture.host, &ids[1]).frames > before,
        "sibling progresses"
    );
}

/// lotus#42: a request that needs a feature the worker did not list in `hello.features` is
/// refused with `Unsupported` before it is sent; listed features pass through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_needing_unadvertised_features_are_refused() {
    use lotus_protocol::debug::{DebugCommand, GpuCapture};
    let fixture = host("features", 1);
    let id = fixture.running(1).await.remove(0);
    let hash = DebugCommand::StateHash {
        chunk_kib: 64,
        sections: true,
        dump: None,
    };
    let error = fixture
        .host
        .debug(&id, &hash, Actor::Host)
        .await
        .expect_err("not advertised");
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        error.to_string().contains("does not support state_hash")
            && error
                .to_string()
                .contains("flush_storage, gpu_capture_levels"),
        "{error}"
    );
    let events = DebugCommand::EventsConfigure {
        notify: None,
        history: None,
    };
    assert_eq!(
        fixture
            .host
            .debug(&id, &events, Actor::Host)
            .await
            .expect_err("not advertised")
            .kind(),
        std::io::ErrorKind::Unsupported
    );
    let counters = DebugCommand::GpuConfigure {
        enabled: true,
        stop_events: vec![],
        capture: Some(GpuCapture::Counters),
        draw_range: None,
        max_draws: None,
        resources: None,
    };
    fixture
        .host
        .debug(&id, &counters, Actor::Host)
        .await
        .expect("advertised");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.root(&id).join("native-manifest.json")).expect("manifest"),
    )
    .expect("json");
    assert_eq!(
        manifest["features"],
        serde_json::json!(["flush_storage", "gpu_capture_levels", "analog_input"])
    );
}

/// lotus#50: a session whose worker names a known flavor records registry drift in its
/// manifest; an unknown flavor records none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_start_records_settings_drift() {
    let fixture = host("drift", 2);
    let store = fixture.base.join("store").canonicalize().expect("store");
    let manifest = |root: std::path::PathBuf| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(root.join("native-manifest.json")).expect("manifest"))
            .expect("json")
    };
    let card = store.join("cards/azahar");
    std::fs::create_dir_all(&card).expect("card");
    std::fs::write(card.join("mock-backend-name"), "AZAHAR").expect("flavor");
    fixture
        .host
        .create(
            SessionKind::Primary {
                card: "azahar".into(),
            },
            Some("demo"),
            Actor::Host,
        )
        .await
        .expect("azahar");
    // The mock lists only settings it was sent (graphics_api from prefer_vulkan), so every
    // other registry key is reported missing; it lists nothing the registry lacks.
    let drift = &manifest(card)["settings_drift"];
    assert_eq!(drift["unknown_to_registry"], serde_json::json!([]));
    let missing = drift["missing_from_worker"].as_array().expect("missing");
    assert!(missing.len() >= 171, "{}", missing.len());
    assert!(!missing.contains(&serde_json::json!("graphics_api")));
    let plain = fixture.running(1).await;
    assert_eq!(
        manifest(fixture.root(&plain[0]))["settings_drift"],
        serde_json::Value::Null
    );
}

/// lotus#67: the workspace is refused headless, needs control from MCP, never touches a
/// worker, and collapses to the sibling when a tiled session stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_arranges_without_touching_workers() {
    use lotus_session::workspace::{Layout, Operation, SplitDefaults};
    let fixture = host("workspace", 2);
    let ids = fixture.running(2).await;
    let tile = Operation::Tile {
        first: ids[0].clone(),
        second: ids[1].clone(),
        axis: None,
        ratio: None,
        screens: None,
    };
    let headless = fixture.host.update_workspace(&tile, None, Actor::Mcp);
    assert_eq!(
        headless.unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert!(!fixture.host.workspace().available);

    fixture.host.enable_workspace(SplitDefaults::default());
    let frames = |id: &str| telemetry(&fixture.host, id);
    let before: Vec<_> = ids.iter().map(|id| frames(id).state).collect();
    let committed = fixture
        .host
        .update_workspace(&tile, Some(0), Actor::Mcp)
        .unwrap();
    assert_eq!(committed.revision, 1);
    fixture
        .host
        .update_workspace(&Operation::Swap, None, Actor::Desktop)
        .unwrap();
    let stale = fixture.host.update_workspace(&tile, Some(1), Actor::Mcp);
    assert_eq!(stale.unwrap_err().kind(), std::io::ErrorKind::ResourceBusy);
    let after: Vec<_> = ids.iter().map(|id| frames(id).state).collect();
    assert_eq!(before, after);

    fixture.host.stop(&ids[0], Actor::Host).await.unwrap();
    let status = fixture.host.workspace();
    assert!(
        matches!(&status.snapshot.layout, Layout::Single { session, .. } if *session == ids[1])
    );
    assert_eq!(status.snapshot.revision, 3);
    assert!(frames(&ids[1]).state.is_active());
    let gone = fixture.host.update_workspace(
        &Operation::Single {
            session: ids[0].clone(),
        },
        None,
        Actor::Desktop,
    );
    assert_eq!(gone.unwrap_err().kind(), std::io::ErrorKind::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn workspace_writes_from_mcp_need_control() {
    use lotus_session::workspace::{Operation, SplitDefaults};
    let fixture = host_with("workspace-ro", 1, |config| config.allow_control = false);
    fixture.host.enable_workspace(SplitDefaults::default());
    let refused = fixture
        .host
        .update_workspace(&Operation::Swap, None, Actor::Mcp);
    assert_eq!(
        refused.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(fixture.host.workspace().available);
}
