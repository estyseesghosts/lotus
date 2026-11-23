//! lotus#95: desktop keyboard and touch input and agent marks combine in one recording,
//! against the real supervisor, desktop input routing and `lotus-mock-native` workers. A
//! keyboard focus change to another session neither reaches nor retargets the recording.
//! The recording operations are the ones the desktop Record Test window and MCP
//! `lotus_record_test` both call (`lotus_mcp::server::record_test_operation`).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lotus_desktop::input_routing::InputRouting;
use lotus_input::router::bits;
use lotus_mcp::server::record_test_operation;
use lotus_mcp::tool_arguments::{RecordTestOperation, RecordTestRegion};
use lotus_protocol::envelope::SessionUuid;
use lotus_session::supervisor::{HostConfig, SessionKind, Supervisor};
use lotus_session::telemetry::Actor;
use lotus_session::test_recording::{InputSource, TestRecording};

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

/// Buttons of every input the mock worker accepted.
fn buttons(root: &Path) -> Vec<u64> {
    std::fs::read_to_string(root.join("inputs.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).expect("json");
            value["buttons"].as_u64().expect("buttons")
        })
        .collect()
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "deadline waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn desktop_input_and_agent_marks_share_one_recording() {
    let base = std::env::temp_dir().join(format!("lotus-record-{}", SessionUuid::generate()));
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
    let operation = |session: &str, op: RecordTestOperation, actor: Actor| {
        runtime.block_on(record_test_operation(&host, session, op, actor))
    };
    let recording = |session: &str| -> Option<TestRecording> {
        host.record_test(session).expect("record_test")
    };

    // The desktop starts the recording on A; the reload leaves A paused, so play it.
    operation(a, RecordTestOperation::Start, Actor::Desktop).expect("start");
    runtime.block_on(host.run(a, Actor::Desktop)).expect("run");
    let routing = InputRouting::new(host.clone());

    routing.keyboard(Some(a), bits::A);
    until("keyboard A recorded", || {
        recording(a).is_some_and(|r| r.inputs.iter().any(|i| i.state.buttons == bits::A))
    });
    // An agent mark between desktop inputs: it pauses at a frame boundary and resumes.
    let agent = operation(
        a,
        RecordTestOperation::Mark {
            label: "agent".into(),
            memory: Vec::new(),
            regions: vec![RecordTestRegion {
                screen: "top".into(),
                x: 0,
                y: 0,
                width: 40,
                height: 20,
            }],
        },
        Actor::Mcp,
    )
    .expect("agent mark");
    assert_eq!(agent["regions"].as_array().map(Vec::len), Some(1));
    let summary = |id: &str| {
        host.summaries()
            .into_iter()
            .find(|s| s.info.id == id)
            .expect("summary")
    };
    assert!(summary(a).telemetry.playing, "the mark resumed playback");

    routing.keyboard(Some(a), 0);
    routing.pointer(Some(a), Some((100, 120)));
    until("touch recorded", || {
        recording(a).is_some_and(|r| r.inputs.iter().any(|i| i.state.touch))
    });
    routing.pointer(Some(a), None);

    // Keyboard focus moves to B. B's input is not A's, and A stays the recording target.
    let before = recording(a).expect("recording").inputs.len();
    routing.keyboard(Some(b), bits::X);
    until("X reached B", || {
        buttons(&root(b)).last() == Some(&u64::from(bits::X))
    });
    routing.keyboard(None, 0);
    assert!(
        recording(b).is_none(),
        "focus change started a recording on B"
    );
    let after = recording(a).expect("recording");
    assert!(
        after.inputs[before..]
            .iter()
            .all(|input| input.state.buttons & bits::X == 0),
        "B's key reached A's recording"
    );

    operation(
        a,
        RecordTestOperation::Mark {
            label: "desktop".into(),
            memory: Vec::new(),
            regions: Vec::new(),
        },
        Actor::Desktop,
    )
    .expect("desktop mark");
    operation(a, RecordTestOperation::Stop, Actor::Mcp).expect("stop");
    let done = recording(a).expect("recording");
    assert!(done.invalid.is_none(), "{:?}", done.invalid);
    assert!(done.inputs.iter().all(|i| i.source == InputSource::Desktop));
    assert_eq!(
        done.marks.iter().map(|m| m.source).collect::<Vec<_>>(),
        [InputSource::Mcp, InputSource::Desktop]
    );
    assert!(done.marks[0].frame <= done.marks[1].frame);
    assert!(done.interventions.is_empty());
    let input = summary(a).telemetry.input;
    assert_eq!((input.denied, input.failed), (0, 0), "ownership collision");
    // Every recorded input frame is at or before the stop frame.
    let stop = done.stopped_at_frame.expect("stopped");
    assert!(done.inputs.iter().all(|i| i.frame <= stop));

    runtime.block_on(host.close());
    let _ = std::fs::remove_dir_all(&base);
}
