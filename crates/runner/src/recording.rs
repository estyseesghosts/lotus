//! Export a live test recording (lotus#95) as a version 2 scenario plus a manifest.
//!
//! Inputs become a frame-anchored `input.schedule`: each accepted state is replayed while
//! paused at its reply's `input_frame`, the frame boundary where the live worker applied it
//! (`lotus_session::test_recording`). Marks become `frame_hash` and `memory` assertions
//! with a capture at their frame. The scenario names its content by file name and SHA-256
//! only; the 3DSX is never copied. Anything the runner cannot reproduce is listed in the
//! manifest's `conditions` instead of being claimed as replayable.
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use lotus_session::native_session::InputState;
use lotus_session::test_recording::{RecordedInput, TestRecording};
use lotus_session::worker::WorkerFlavor;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    Assertion, Capture, CaptureWhen, Content, Input, InputEvent, Outputs, Profile, Scenario, Stop,
    Storage,
};

/// Button names the runner accepts, by `InputState::buttons` bit (`execution.rs`
/// `button_mask`, Azahar `Settings::NativeButton`).
const BUTTONS: [(u32, &str); 14] = [
    (0, "a"),
    (1, "b"),
    (2, "x"),
    (3, "y"),
    (4, "up"),
    (5, "down"),
    (6, "left"),
    (7, "right"),
    (8, "l"),
    (9, "r"),
    (10, "start"),
    (11, "select"),
    (14, "zl"),
    (15, "zr"),
];

/// Exports go to `<storage_root>/recorded-tests/<name>/`.
pub const DIRECTORY: &str = "recorded-tests";

/// Edits applied while exporting.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportOptions {
    /// Directory and scenario name: 1 to 64 letters, digits, `_` or `-`.
    pub name: String,
    /// Last frame to run, 1 up to the recording's stop frame (default). Inputs at or after
    /// it are dropped, which trims the end of the recording.
    #[serde(default)]
    pub stop_frame: Option<u64>,
    /// Indices of marks to leave out.
    #[serde(default)]
    pub omit_marks: Vec<usize>,
    /// Indices of marks whose whole-screen hashes are left out; their region and memory
    /// assertions stay. For screens with a clock or animation outside the checked regions.
    #[serde(default)]
    pub omit_screen_hashes: Vec<usize>,
    /// Extra version 2 assertions, validated with the rest of the scenario.
    #[serde(default)]
    pub assertions: Vec<Assertion>,
}

#[derive(Debug, Clone)]
pub struct Export {
    pub scenario: Scenario,
    pub yaml: String,
    pub manifest: Value,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn condition(code: &str, detail: String) -> Value {
    json!({"code": code, "detail": detail})
}

/// Valid export name: also a directory name.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn event(frame: u64, button: &str, action: &str, point: Option<(u16, u16)>) -> InputEvent {
    InputEvent {
        at_frame: frame,
        button: button.into(),
        action: action.into(),
        x: point.map(|point| point.0),
        y: point.map(|point| point.1),
    }
}

/// Button and touch events that take `before` to `after`.
fn transition(frame: u64, before: &InputState, after: &InputState, events: &mut Vec<InputEvent>) {
    for (bit, name) in BUTTONS {
        let mask = 1 << bit;
        if before.buttons & mask != after.buttons & mask {
            let action = if after.buttons & mask != 0 {
                "down"
            } else {
                "up"
            };
            events.push(event(frame, name, action, None));
        }
    }
    let moved = (before.x, before.y) != (after.x, after.y);
    if after.touch && (!before.touch || moved) {
        events.push(event(frame, "touch", "down", Some((after.x, after.y))));
    } else if before.touch && !after.touch {
        events.push(event(frame, "touch", "up", Some((before.x, before.y))));
    }
}

/// The state the worker holds after each frame boundary: the last input accepted there.
fn final_states(inputs: &[RecordedInput], stop: u64) -> BTreeMap<u64, &RecordedInput> {
    let mut states = BTreeMap::new();
    for input in inputs.iter().filter(|input| input.frame < stop) {
        states.insert(input.frame, input);
    }
    states
}

/// Build the scenario and manifest. Fails for an active or invalid recording and for edits
/// that do not fit it.
pub fn export(recording: &TestRecording, options: &ExportOptions) -> io::Result<Export> {
    if !valid_name(&options.name) {
        return Err(invalid(
            "export name must be 1 to 64 letters, digits, '_' or '-'",
        ));
    }
    if let Some(reason) = &recording.invalid {
        return Err(invalid(format!(
            "the recording cannot be exported: {reason}"
        )));
    }
    let stopped = recording
        .stopped_at_frame
        .ok_or_else(|| invalid("stop the recording before exporting it"))?;
    let stop = options.stop_frame.unwrap_or(stopped);
    if stop == 0 || stop > stopped {
        return Err(invalid(format!("stop_frame must be 1..={stopped}")));
    }
    let flavor = match recording.worker["backend"].as_str() {
        Some("AZAHAR") => WorkerFlavor::Integration,
        Some("CITnLO") => WorkerFlavor::Experimental,
        other => {
            return Err(invalid(format!(
                "the scenario runner needs an AZAHAR or CITnLO worker, not {}",
                other.unwrap_or("an unnamed backend")
            )))
        }
    };
    if let Some(index) = options
        .omit_marks
        .iter()
        .chain(&options.omit_screen_hashes)
        .find(|index| **index >= recording.marks.len())
    {
        return Err(invalid(format!("no mark {index} to omit")));
    }
    let mut conditions = Vec::new();

    // Inputs: one transition per frame boundary, from the released state at frame 0.
    let mut schedule = Vec::new();
    let mut held = InputState::default();
    let mut analog = Vec::new();
    let mut unnamed = 0u32;
    let named = BUTTONS
        .iter()
        .fold(0u32, |mask, (bit, _)| mask | (1 << bit));
    for (frame, input) in final_states(&recording.inputs, stop) {
        let mut state = input.state;
        if state.analog() {
            analog.push(frame);
        }
        unnamed |= state.buttons & !named;
        state.buttons &= named;
        state.circle_pad = [0.0; 2];
        state.c_stick = [0.0; 2];
        transition(frame, &held, &state, &mut schedule);
        held = state;
    }
    if schedule.len() > 4096 {
        return Err(invalid(format!(
            "{} input events exceed the runner's 4,096; export a shorter stop_frame",
            schedule.len()
        )));
    }
    if !analog.is_empty() {
        conditions.push(condition(
            "analog_input",
            format!(
                "circle pad or C-stick input at {} frame(s) from {} is not replayed: scenario input has buttons and touch only",
                analog.len(),
                analog[0]
            ),
        ));
    }
    if unnamed != 0 {
        conditions.push(condition(
            "unnamed_buttons",
            format!("button bits {unnamed:#x} have no scenario name and are not replayed"),
        ));
    }

    // Marks: frame hashes need the runner's software renderer; memory holds anywhere.
    let hashes = recording.renderer == "software";
    if !hashes {
        conditions.push(condition(
            "renderer",
            format!(
                "recorded on {}; the runner replays on software, so screen hashes were left out",
                recording.renderer
            ),
        ));
    }
    let mut assertions = Vec::new();
    let mut captures = Vec::new();
    let mut marks = Vec::new();
    for (index, mark) in recording.marks.iter().enumerate() {
        let omitted = options.omit_marks.contains(&index);
        let skipped = if omitted {
            Some("omitted")
        } else if mark.frame == 0 {
            Some("at frame 0, before the first frame the runner checks")
        } else if mark.frame > stop {
            Some("after stop_frame")
        } else {
            None
        };
        if let Some(reason) = skipped {
            marks.push(json!({"label": mark.label, "frame": mark.frame, "skipped": reason}));
            continue;
        }
        let first = assertions.len();
        if hashes {
            if !options.omit_screen_hashes.contains(&index) {
                for (screen, sha256) in [("top", &mark.top_sha256), ("bottom", &mark.bottom_sha256)]
                {
                    assertions.push(Assertion {
                        kind: "frame_hash".into(),
                        at_frame: Some(mark.frame),
                        screen: Some(screen.into()),
                        sha256: Some(sha256.clone()),
                        ..Assertion::default()
                    });
                }
            }
            for region in &mark.regions {
                assertions.push(Assertion {
                    kind: "frame_hash".into(),
                    at_frame: Some(mark.frame),
                    screen: Some(region.screen.clone()),
                    sha256: Some(region.sha256.clone()),
                    region: Some(region.region),
                    ..Assertion::default()
                });
            }
            if assertions.len() > first
                && mark.frame != stop
                && !captures.iter().any(|capture: &Capture| {
                    capture.when.as_ref().and_then(|when| when.frame) == Some(mark.frame)
                })
            {
                captures.push(Capture {
                    when: Some(CaptureWhen {
                        frame: Some(mark.frame),
                        event: None,
                        text_contains: None,
                    }),
                    include: vec!["top_raw_frame".into(), "bottom_raw_frame".into()],
                    max_bytes: None,
                });
            }
        }
        for value in &mark.memory {
            assertions.push(Assertion {
                kind: "memory".into(),
                at_frame: Some(mark.frame),
                address: Some(value.address),
                width: Some(u32::from(value.width)),
                equals: Some(u64::from(value.value)),
                ..Assertion::default()
            });
        }
        marks.push(json!({"label": mark.label, "frame": mark.frame,
            "assertions": (first..assertions.len()).collect::<Vec<_>>()}));
    }
    assertions.push(Assertion {
        kind: "frame_count".into(),
        minimum: Some(stop),
        ..Assertion::default()
    });
    let authored = assertions.len();
    assertions.extend(options.assertions.iter().cloned());

    if !recording.determinism {
        conditions.push(condition(
            "determinism",
            "the determinism profile was off: asynchronous HLE operations may finish on other frames when replayed (docs/09, #29)".into(),
        ));
    }
    if !recording.card_storage.is_empty() {
        conditions.push(condition(
            "card_storage",
            format!(
                "guest storage held {} file(s) a fresh root does not have ({}{}); the runner starts from empty storage",
                recording.card_storage.len() as u64 + recording.card_storage_more,
                recording.card_storage.join(", "),
                if recording.card_storage_more > 0 { ", ..." } else { "" }
            ),
        ));
    }
    if !recording.other_settings.is_empty() {
        conditions.push(condition(
            "settings",
            format!(
                "settings differ from the worker default and are not applied by the runner: {}",
                recording
                    .other_settings
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    if !recording.interventions.is_empty() || recording.interventions_dropped > 0 {
        let mut operations: Vec<&str> = recording
            .interventions
            .iter()
            .map(|item| item.operation.as_str())
            .collect();
        operations.dedup();
        conditions.push(condition(
            "interventions",
            format!(
                "{} debugger change(s) during recording are not replayed: {}",
                recording.interventions.len() as u64 + recording.interventions_dropped,
                operations.join(", ")
            ),
        ));
    }

    // Software renders far slower than real time; allow 10 frames per second plus boot.
    let wall_timeout_seconds = (stop.div_ceil(10) + 60).min(crate::MAX_SCENARIO_WALL_SECONDS);
    let scenario = Scenario {
        version: 2,
        name: options.name.clone(),
        content: Content {
            kind: "homebrew_3dsx".into(),
            path_handle: recording.content_name.clone(),
            sha256: recording.content["sha256"].as_str().map(str::to_string),
        },
        profile: Profile {
            model: recording.model.clone(),
            renderer: "software".into(),
            worker_flavor: flavor,
            worker_sha256: recording.worker["worker_sha256"]
                .as_str()
                .map(str::to_string),
            deterministic_initial_ticks: Some(0),
            audio_output: "discard".into(),
            determinism: recording.determinism,
        },
        storage: Storage {
            mode: "isolated_copy".into(),
            base_snapshot: None,
        },
        input: Input {
            owner: "scenario".into(),
            schedule,
        },
        stop: Stop {
            max_frames: stop,
            wall_timeout_seconds,
        },
        movie: None,
        captures,
        assertions,
        outputs: Outputs::default(),
    };
    scenario
        .validate()
        .map_err(|error| invalid(error.to_string()))?;
    crate::execution::validate_execution(&scenario)?;
    let yaml = serde_yaml::to_string(&scenario).map_err(io::Error::other)?;
    let manifest = json!({
        "format": "lotus-recorded-test",
        "version": 1,
        "name": options.name,
        "recording": recording.id,
        "session": recording.session,
        "started_unix_ms": recording.started_unix_ms,
        "content": {"name": recording.content_name, "sha256": recording.content["sha256"],
            "bytes": recording.content["bytes"],
            "note": "not included; place the 3DSX named here next to scenario.yaml"},
        "worker": recording.worker,
        "profile": {"model": recording.model, "renderer_recorded": recording.renderer,
            "renderer_replayed": "software", "determinism": recording.determinism},
        "replay": "frame-anchored: each input state is applied while paused at the frame boundary where the live worker applied it; repeatability depends on the worker's determinism (#29, #36)",
        "recorded_inputs": recording.inputs.len(),
        "schedule_events": scenario.input.schedule.len(),
        "stop_frame": stop,
        "recorded_stop_frame": stopped,
        "marks": marks,
        "authored_assertions": authored,
        "edited_assertions": options.assertions.len(),
        "conditions": conditions,
    });
    Ok(Export {
        scenario,
        yaml,
        manifest,
    })
}

/// Write `scenario.yaml` and `manifest.json` into a new `directory`.
pub fn write(export: &Export, directory: &Path) -> io::Result<()> {
    if let Some(parent) = directory.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir(directory)?;
    std::fs::write(directory.join("scenario.yaml"), &export.yaml)?;
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&export.manifest).map_err(io::Error::other)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lotus_session::native_session::ScreenRegion;
    use lotus_session::test_recording::{
        InputSource, Intervention, MarkValue, RecordedMark, RegionHash,
    };

    fn input(frame: u64, buttons: u32, touch: Option<(u16, u16)>) -> RecordedInput {
        RecordedInput {
            frame,
            state: InputState {
                buttons,
                x: touch.map_or(0, |point| point.0),
                y: touch.map_or(0, |point| point.1),
                touch: touch.is_some(),
                ..InputState::default()
            },
            owner: "live".into(),
            source: InputSource::Desktop,
        }
    }

    fn mark(frame: u64, label: &str) -> RecordedMark {
        RecordedMark {
            frame,
            label: label.into(),
            top_sha256: "a".repeat(64),
            bottom_sha256: "b".repeat(64),
            regions: Vec::new(),
            memory: vec![MarkValue {
                address: 0x0010_0000,
                width: 4,
                value: 7,
            }],
            source: InputSource::Mcp,
        }
    }

    fn recording() -> TestRecording {
        TestRecording {
            id: "rec".into(),
            session: "s".into(),
            session_created_unix_ms: 1,
            epoch: 2,
            started_unix_ms: 3,
            asset: "fixture".into(),
            content_name: "input.3dsx".into(),
            content: json!({"bytes": 10, "sha256": "c".repeat(64)}),
            worker: json!({"source": "abc", "backend": "CITnLO", "worker_sha256": "d".repeat(64)}),
            model: "new3ds".into(),
            renderer: "software".into(),
            determinism: true,
            other_settings: BTreeMap::new(),
            card_storage: Vec::new(),
            card_storage_more: 0,
            inputs: vec![
                input(0, 0, None),
                input(10, 1, None),
                input(10, 1 | 16, None),
                input(20, 16, Some((5, 6))),
                input(25, 0, Some((7, 8))),
                input(30, 0, None),
                input(90, 2, None),
            ],
            marks: vec![mark(40, "first"), mark(60, "second")],
            interventions: Vec::new(),
            interventions_dropped: 0,
            inputs_dropped: 0,
            invalid: None,
            stopped_at_frame: Some(80),
        }
    }

    fn options() -> ExportOptions {
        ExportOptions {
            name: "recorded".into(),
            ..ExportOptions::default()
        }
    }

    fn events(scenario: &Scenario) -> Vec<(u64, &str, &str, Option<u16>)> {
        scenario
            .input
            .schedule
            .iter()
            .map(|event| {
                (
                    event.at_frame,
                    event.button.as_str(),
                    event.action.as_str(),
                    event.x,
                )
            })
            .collect()
    }

    #[test]
    fn inputs_become_transitions_at_their_frames() {
        let export = export(&recording(), &options()).unwrap();
        // Frame 10 keeps only its last state (A + up); frame 90 is past the stop.
        assert_eq!(
            events(&export.scenario),
            vec![
                (10, "a", "down", None),
                (10, "up", "down", None),
                (20, "a", "up", None),
                (20, "touch", "down", Some(5)),
                (25, "up", "up", None),
                (25, "touch", "down", Some(7)),
                (30, "touch", "up", Some(7)),
            ]
        );
        assert_eq!(export.scenario.stop.max_frames, 80);
        assert_eq!(
            export.scenario.profile.worker_flavor,
            WorkerFlavor::Experimental
        );
        assert_eq!(export.scenario.profile.model, "new3ds");
        assert!(export.manifest["conditions"].as_array().unwrap().is_empty());
        // The YAML is what the runner reads back.
        let reread = Scenario::from_yaml(&export.yaml).unwrap();
        assert_eq!(reread, export.scenario);
    }

    #[test]
    fn marks_become_hash_and_memory_assertions_with_captures() {
        let export = export(&recording(), &options()).unwrap();
        let kinds: Vec<(&str, Option<u64>)> = export
            .scenario
            .assertions
            .iter()
            .map(|a| (a.kind.as_str(), a.at_frame))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("frame_hash", Some(40)),
                ("frame_hash", Some(40)),
                ("memory", Some(40)),
                ("frame_hash", Some(60)),
                ("frame_hash", Some(60)),
                ("memory", Some(60)),
                ("frame_count", None),
            ]
        );
        assert_eq!(export.scenario.assertions[2].equals, Some(7));
        assert_eq!(export.scenario.captures.len(), 2);
        assert_eq!(export.manifest["marks"][1]["assertions"], json!([3, 4, 5]));
    }

    #[test]
    fn mark_regions_become_region_hashes_and_can_stand_alone() {
        let mut recording = recording();
        let region = ScreenRegion {
            x: 8,
            y: 16,
            width: 32,
            height: 24,
        };
        recording.marks[0].regions.push(RegionHash {
            screen: "bottom".into(),
            region,
            sha256: "e".repeat(64),
        });
        let export = export(
            &recording,
            &ExportOptions {
                omit_screen_hashes: vec![0],
                ..options()
            },
        )
        .unwrap();
        let first = &export.scenario.assertions[0];
        assert_eq!(
            (first.kind.as_str(), first.at_frame, first.screen.as_deref()),
            ("frame_hash", Some(40), Some("bottom"))
        );
        assert_eq!(first.region, Some(region));
        assert_eq!(export.scenario.assertions[1].kind, "memory");
        assert_eq!(export.manifest["marks"][0]["assertions"], json!([0, 1]));
        // The region assertion still needs, and gets, the frame capture.
        assert_eq!(export.scenario.captures.len(), 2);
        assert!(export.yaml.contains("region:"), "{}", export.yaml);
        // A mark with only memory left needs no capture.
        let memory_only = super::export(
            &self::recording(),
            &ExportOptions {
                omit_screen_hashes: vec![0, 1],
                ..options()
            },
        )
        .unwrap();
        assert!(memory_only.scenario.captures.is_empty());
        assert!(super::export(
            &self::recording(),
            &ExportOptions {
                omit_screen_hashes: vec![2],
                ..options()
            }
        )
        .is_err());
    }

    #[test]
    fn trimming_and_omitting_edit_the_export() {
        let options = ExportOptions {
            stop_frame: Some(50),
            omit_marks: vec![0],
            ..options()
        };
        let export = export(&recording(), &options).unwrap();
        assert_eq!(export.scenario.stop.max_frames, 50);
        assert_eq!(export.manifest["marks"][0]["skipped"], "omitted");
        assert_eq!(export.manifest["marks"][1]["skipped"], "after stop_frame");
        assert_eq!(export.scenario.assertions.len(), 1, "frame_count only");
        assert!(super::export(
            &recording(),
            &ExportOptions {
                stop_frame: Some(81),
                ..ExportOptions::default()
            }
        )
        .is_err());
    }

    #[test]
    fn unreplayable_conditions_are_reported() {
        let mut recording = recording();
        recording.renderer = "vulkan".into();
        recording.determinism = false;
        recording.card_storage = vec!["sdmc/save.bin".into()];
        recording
            .other_settings
            .insert("region_value".into(), json!({}));
        recording.inputs.push(RecordedInput {
            state: InputState {
                circle_pad: [0.5, 0.0],
                buttons: 1 << 12,
                ..InputState::default()
            },
            ..input(35, 0, None)
        });
        recording.interventions.push(Intervention {
            observed_frame: 33,
            operation: "write_memory".into(),
            source: InputSource::Mcp,
        });
        let export = export(&recording, &options()).unwrap();
        let codes: Vec<&str> = export.manifest["conditions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|condition| condition["code"].as_str().unwrap())
            .collect();
        assert_eq!(
            codes,
            vec![
                "analog_input",
                "unnamed_buttons",
                "renderer",
                "determinism",
                "card_storage",
                "settings",
                "interventions"
            ]
        );
        assert!(export
            .scenario
            .assertions
            .iter()
            .all(|assertion| assertion.kind != "frame_hash"));
        assert!(export.scenario.captures.is_empty());
    }

    #[test]
    fn active_invalid_and_unknown_recordings_are_refused() {
        let mut active = recording();
        active.stopped_at_frame = None;
        assert!(export(&active, &options()).is_err());
        let mut invalid = recording();
        invalid.invalid = Some("restarted".into());
        assert!(export(&invalid, &options()).is_err());
        let mut mock = recording();
        mock.worker["backend"] = "mock-native".into();
        assert!(export(&mock, &options()).is_err());
        assert!(export(
            &recording(),
            &ExportOptions {
                name: "../escape".into(),
                ..ExportOptions::default()
            }
        )
        .is_err());
    }
}
