//! lotus#95 acceptance: a live recording on a native worker, exported, passes the scenario
//! runner with the assertions taken live. Opt-in: needs `LOTUS_NATIVE_WORKER` and
//! `LOTUS_INPUT_FIXTURE` (the owned `fixtures/input` 3DSX, built by
//! `tools/build-debug-fixture.py input`, with its ELF next to it) and `llvm-nm` on PATH.
//!
//! The fixture folds every HID pad and touch entry into `checksum` (fixtures/input/fixture.c),
//! so a replay that applies any input on another HID update fails the mark's memory
//! assertion, not only the screen hashes.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lotus_protocol::debug::SettingValue;
use lotus_runner::recording::{export, write, ExportOptions};
use lotus_session::native_session::{InputState, ScreenRegion};
use lotus_session::supervisor::{HostConfig, SessionKind, Supervisor};
use lotus_session::telemetry::Actor;
use lotus_session::test_recording::{MarkRead, MarkRegion};

const WATCHED: [&str; 5] = ["stage", "failure", "updates", "transitions", "checksum"];

fn symbol(elf: &Path, name: &str) -> u32 {
    let output = Command::new("llvm-nm").arg(elf).output().expect("llvm-nm");
    let text = String::from_utf8(output.stdout).unwrap();
    let line = text
        .lines()
        .find(|line| line.ends_with(&format!(" {name}")))
        .unwrap_or_else(|| panic!("{name} not in {}", elf.display()));
    u32::from_str_radix(&line[..8], 16).unwrap()
}

/// Fixed-seed sequence so a failure reproduces with the same live input shape.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires LOTUS_NATIVE_WORKER and LOTUS_INPUT_FIXTURE with an adjacent ELF"]
async fn a_recorded_session_replays_through_the_scenario_runner() {
    let worker = PathBuf::from(std::env::var_os("LOTUS_NATIVE_WORKER").unwrap());
    let fixture = PathBuf::from(std::env::var_os("LOTUS_INPUT_FIXTURE").unwrap());
    let elf = fixture.with_extension("elf");
    let base = std::env::temp_dir().join(format!("lotus-record-95-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    let mut config = HostConfig::new(&worker, base.join("host"));
    config.allow_control = true;
    config.prefer_vulkan = false;
    config.assets.insert("input".into(), fixture.clone());
    // The runner's determinism profile (runner `prepare`, docs/09).
    config
        .profile
        .insert("graphics_api".into(), SettingValue::Integer(0));
    config.profile.insert(
        "deterministic_async_operations".into(),
        SettingValue::Boolean(true),
    );
    config
        .profile
        .insert("async_fs_operations".into(), SettingValue::Boolean(false));
    let host = Supervisor::new(config).unwrap();
    let id = host
        .create(
            SessionKind::Secondary { parent: None },
            Some("input"),
            Actor::Host,
        )
        .await
        .unwrap()
        .id;

    let started = host.record_test_start(&id, Actor::Desktop).await.unwrap();
    assert_eq!(started.renderer, "software");
    assert!(started.determinism);
    host.run(&id, Actor::Host).await.unwrap();

    let reads: Vec<MarkRead> = WATCHED
        .iter()
        .map(|name| MarkRead {
            address: symbol(&elf, name),
            width: 4,
        })
        .collect();
    let half = |screen: &str, y, width| MarkRegion {
        screen: screen.into(),
        region: ScreenRegion {
            x: 0,
            y,
            width,
            height: 120,
        },
    };
    let mut rng = Lcg(95);
    // About 10 s of live input: buttons, touch and releases at irregular wall times.
    for step in 0..60 {
        tokio::time::sleep(Duration::from_millis(60 + rng.next(220))).await;
        let state = InputState {
            buttons: [0, 1, 2, 16, 1 | 16, 1 << 10][rng.next(6) as usize],
            touch: rng.next(10) < 3,
            x: rng.next(320) as u16,
            y: rng.next(240) as u16,
            ..InputState::default()
        };
        host.input(&id, state, "live", Actor::Mcp).await.unwrap();
        if step == 30 {
            host.record_test_mark(
                &id,
                "midway",
                &reads,
                &[half("bottom", 120, 320)],
                Actor::Mcp,
            )
            .await
            .unwrap();
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let end = host
        .record_test_mark(&id, "end", &reads, &[half("top", 0, 400)], Actor::Mcp)
        .await
        .unwrap();
    let recording = host.record_test_stop(&id, Actor::Mcp).await.unwrap();
    let changes = host
        .memory(&id, symbol(&elf, "changes"), 64 * 12)
        .await
        .unwrap();
    let changes: Vec<u8> = serde_json::from_value(changes["bytes"].clone()).unwrap();
    let words: Vec<u32> = changes
        .chunks(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect();
    eprintln!("live changes {:?}", words.chunks(3).collect::<Vec<_>>());
    host.stop(&id, Actor::Host).await.unwrap();

    let value =
        |name: &str| end.memory[WATCHED.iter().position(|watched| *watched == name).unwrap()].value;
    assert_eq!(value("stage"), 2, "fixture mapped HID memory");
    assert_eq!(value("failure"), 0);
    assert!(value("transitions") > 10, "the guest saw the live input");
    assert!(recording.inputs.len() > 50, "{}", recording.inputs.len());
    assert!(recording.interventions.is_empty());
    eprintln!(
        "recorded {} inputs, marks at {:?}, stop {:?}, end checksum {:#x} after {} updates",
        recording.inputs.len(),
        recording
            .marks
            .iter()
            .map(|mark| mark.frame)
            .collect::<Vec<_>>(),
        recording.stopped_at_frame,
        value("checksum"),
        value("updates"),
    );

    let exported = export(
        &recording,
        &ExportOptions {
            name: "input-95".into(),
            // The end mark is checked by its region and memory only.
            omit_screen_hashes: vec![1],
            ..ExportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        exported.manifest["conditions"],
        serde_json::json!([]),
        "{}",
        exported.manifest
    );
    let directory = base.join("recorded-tests").join("input-95");
    write(&exported, &directory).unwrap();
    std::fs::copy(&fixture, directory.join(&recording.content_name)).unwrap();

    let scenario = lotus_runner::Scenario::load(directory.join("scenario.yaml")).unwrap();
    let report = lotus_runner::execution::run(&scenario, &directory, &worker, &base.join("run"))
        .await
        .unwrap_or_else(|error| panic!("replay failed: {error}"));
    assert!(report.passed);
    // Midway: two screen hashes, one region, five memory values. End: one region, five
    // memory values. Then frame_count.
    assert_eq!(
        report.assertions.len(),
        (3 + WATCHED.len()) + (1 + WATCHED.len()) + 1
    );
    let regions = report
        .assertions
        .iter()
        .filter(|result| result.provenance.contains("region"))
        .count();
    assert_eq!(regions, 2);
    eprintln!(
        "replayed {} frames in {} ms, {} assertions passed",
        report.frames,
        report.host_elapsed_ms,
        report.assertions.len()
    );
    let _ = std::fs::remove_dir_all(&base);
}
