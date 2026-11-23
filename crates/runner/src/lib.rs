//! Scenario definition and validation.
//!
//! A scenario is a reproducible, bounded run: fixed content, fixed input
//! schedule, a stop condition and explicit assertions. Every scenario is
//! validated before it runs, because an unbounded scenario can exhaust the
//! host or hang a test suite.

use lotus_session::worker::WorkerFlavor;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;
pub mod assertions;
pub mod assessment;
#[cfg(windows)]
pub mod bisect;
pub mod execution;
pub mod history;
pub mod recording;
pub mod repro;
pub mod suite;

/// Maximum frames a scenario may request.
pub const MAX_SCENARIO_FRAMES: u64 = 100_000;
/// Maximum wall clock time a scenario may request, in seconds.
pub const MAX_SCENARIO_WALL_SECONDS: u64 = 3_600;
/// Maximum bytes a single capture may request.
pub const MAX_CAPTURE_BYTES: u64 = 512 * 1024 * 1024;

/// Errors raised while loading or validating a scenario.
#[derive(Debug, Error)]
pub enum ScenarioError {
    /// The file could not be read.
    #[error("cannot read scenario {path}: {source}")]
    Io {
        /// Scenario path.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid YAML for this schema.
    #[error("cannot parse scenario: {0}")]
    Parse(#[source] serde_yaml::Error),
    /// The scenario is not valid.
    #[error("invalid scenario: {0}")]
    Invalid(String),
}

/// Content loaded by a scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Content {
    /// Content kind, e.g. `homebrew_3dsx`.
    pub kind: String,
    /// Path of the content relative to the scenario file.
    pub path_handle: String,
    /// Expected SHA-256 of the content, if known.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Emulation profile of a scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Console model.
    pub model: String,
    /// Render backend.
    pub renderer: String,
    /// Azahar branch flavour.
    pub worker_flavor: WorkerFlavor,
    /// Expected worker executable SHA-256, if pinned by the scenario.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_sha256: Option<String>,
    /// Initial deterministic tick value.
    #[serde(default)]
    pub deterministic_initial_ticks: Option<u64>,
    /// Audio output mode; `discard` still runs the DSP.
    #[serde(default = "default_audio_output")]
    pub audio_output: String,
    /// Determinism profile: synchronous async operations and filesystem
    /// (`deterministic_async_operations` on, `async_fs_operations` off) on top
    /// of the worker's fixed initial clock and ticks.
    #[serde(default)]
    pub determinism: bool,
}

/// Movie recorded or replayed by a scenario (CITnLO workers only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Movie {
    /// `record` or `play`.
    pub mode: String,
    /// Movie name: 1..32 letters, digits, `_` or `-`.
    pub name: String,
    /// For `play`: recorded `.ctm` relative to the scenario file; its metadata
    /// `.json` must sit next to it.
    #[serde(default)]
    pub path_handle: Option<String>,
    /// For `play`: refuse any input that is not from the movie.
    #[serde(default = "default_read_only")]
    pub read_only: bool,
}

fn default_read_only() -> bool {
    true
}

fn default_audio_output() -> String {
    "discard".to_string()
}

/// Storage isolation of a scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Storage {
    /// Storage mode, e.g. `isolated_copy`.
    pub mode: String,
    /// Base snapshot the run starts from.
    #[serde(default)]
    pub base_snapshot: Option<String>,
}

/// Input schedule entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputEvent {
    /// Frame the event is injected at.
    pub at_frame: u64,
    /// Button name.
    pub button: String,
    /// `down`, `up` or `press`.
    pub action: String,
    #[serde(default)]
    pub x: Option<u16>,
    #[serde(default)]
    pub y: Option<u16>,
}

/// Input ownership and schedule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    /// Owner of input during the run; `scenario` disables live input.
    pub owner: String,
    /// Scheduled events.
    #[serde(default)]
    pub schedule: Vec<InputEvent>,
}

/// Stop condition of a scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stop {
    /// Maximum number of frames.
    pub max_frames: u64,
    /// Wall clock limit in seconds.
    pub wall_timeout_seconds: u64,
}

/// When a capture is taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureWhen {
    /// Frame the capture is taken at.
    pub frame: Option<u64>,
    /// Event kind that triggers the capture (version 2): the capture is taken at the end of
    /// the first frame in which a matching event is recorded.
    pub event: Option<String>,
    /// Substring an event's `text`, `message` or `reason` must contain.
    #[serde(default)]
    pub text_contains: Option<String>,
}

/// A single capture request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    /// Trigger.
    #[serde(default)]
    pub when: Option<CaptureWhen>,
    /// What to include.
    pub include: Vec<String>,
    /// Maximum size of the capture.
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

/// A scenario assertion.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assertion {
    /// Assertion type, e.g. `event_seen`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Event name for `event_seen`.
    #[serde(default)]
    pub event: Option<String>,
    /// Minimum count for `event_seen`.
    #[serde(default)]
    pub count_at_least: Option<u64>,
    /// Lower bound: `frame_count`, and `memory`/`register`/`event_count` ranges.
    #[serde(default)]
    pub minimum: Option<u64>,
    /// Upper bound of a `memory`/`register`/`event_count` range.
    #[serde(default)]
    pub maximum: Option<u64>,
    /// Exact expected value of a `memory`/`register` read.
    #[serde(default)]
    pub equals: Option<u64>,
    /// Frame checkpoint of a `memory`, `register` or `frame_hash` assertion.
    #[serde(default)]
    pub at_frame: Option<u64>,
    /// Guest virtual address: `memory` reads, `reached`/`not_reached` targets.
    #[serde(default)]
    pub address: Option<u32>,
    /// `memory` read width in bytes: 1, 2 or 4 (default 4), little-endian.
    #[serde(default)]
    pub width: Option<u32>,
    /// `register` core (default 0).
    #[serde(default)]
    pub core: Option<u32>,
    /// `register` index: r0..r15, 16 = cpsr.
    #[serde(default)]
    pub register: Option<u32>,
    /// Event filter substring of `text`, `message` or `reason`.
    #[serde(default)]
    pub text_contains: Option<String>,
    /// `frame_hash` screen: `top` or `bottom`.
    #[serde(default)]
    pub screen: Option<String>,
    /// `frame_hash` expected SHA-256 of the raw BGRA capture.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Output retention policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outputs {
    /// Artifacts that are retained.
    pub retain: Vec<String>,
}

/// A complete scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scenario {
    /// Scenario schema version.
    pub version: u32,
    /// Scenario name.
    pub name: String,
    /// Content to load.
    pub content: Content,
    /// Emulation profile.
    pub profile: Profile,
    /// Storage isolation.
    pub storage: Storage,
    /// Input schedule.
    pub input: Input,
    /// Stop condition.
    pub stop: Stop,
    /// Movie recording or playback.
    #[serde(default)]
    pub movie: Option<Movie>,
    /// Captures.
    #[serde(default)]
    pub captures: Vec<Capture>,
    /// Assertions.
    #[serde(default)]
    pub assertions: Vec<Assertion>,
    /// Output retention.
    #[serde(default)]
    pub outputs: Outputs,
}

impl Scenario {
    /// Load a scenario from a YAML file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ScenarioError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|source| ScenarioError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_yaml(&text)
    }

    /// Decode a scenario document already read by the caller.
    pub fn from_yaml(text: &str) -> Result<Self, ScenarioError> {
        serde_yaml::from_str(text).map_err(ScenarioError::Parse)
    }

    /// Validate the scenario against the project's execution limits.
    pub fn validate(&self) -> Result<(), ScenarioError> {
        if !matches!(self.version, 1 | 2) {
            return Err(ScenarioError::Invalid(format!(
                "unsupported scenario version {}",
                self.version
            )));
        }
        if self.name.trim().is_empty() {
            return Err(ScenarioError::Invalid("scenario name is empty".to_string()));
        }
        if self.stop.max_frames == 0 {
            return Err(ScenarioError::Invalid(
                "stop.max_frames must be greater than zero".to_string(),
            ));
        }
        if self.stop.max_frames > MAX_SCENARIO_FRAMES {
            return Err(ScenarioError::Invalid(format!(
                "stop.max_frames {} exceeds the limit of {MAX_SCENARIO_FRAMES}",
                self.stop.max_frames
            )));
        }
        if self.stop.wall_timeout_seconds == 0 {
            return Err(ScenarioError::Invalid(
                "stop.wall_timeout_seconds must be greater than zero".to_string(),
            ));
        }
        if self.stop.wall_timeout_seconds > MAX_SCENARIO_WALL_SECONDS {
            return Err(ScenarioError::Invalid(format!(
                "stop.wall_timeout_seconds {} exceeds the limit of {MAX_SCENARIO_WALL_SECONDS}",
                self.stop.wall_timeout_seconds
            )));
        }

        let mut previous_frame = 0u64;
        for event in &self.input.schedule {
            if event.at_frame > self.stop.max_frames {
                return Err(ScenarioError::Invalid(format!(
                    "input event at frame {} is beyond stop.max_frames {}",
                    event.at_frame, self.stop.max_frames
                )));
            }
            if event.at_frame < previous_frame {
                return Err(ScenarioError::Invalid(
                    "input schedule is not ordered by frame".to_string(),
                ));
            }
            previous_frame = event.at_frame;
        }

        for capture in &self.captures {
            if let Some(max_bytes) = capture.max_bytes {
                if max_bytes > MAX_CAPTURE_BYTES {
                    return Err(ScenarioError::Invalid(format!(
                        "capture max_bytes {max_bytes} exceeds the limit of {MAX_CAPTURE_BYTES}"
                    )));
                }
            }
        }

        if self.version == 2 {
            return assertions::validate(self);
        }
        for capture in &self.captures {
            if capture
                .when
                .as_ref()
                .is_some_and(|when| when.text_contains.is_some())
            {
                return Err(ScenarioError::Invalid(
                    "capture text_contains needs scenario version 2".to_string(),
                ));
            }
        }
        for assertion in &self.assertions {
            match assertion.kind.as_str() {
                // Version 1 named lotus-trace categories; it never executed (lotus#94).
                "event_seen" => {
                    let event = assertion.event.as_deref().ok_or_else(|| {
                        ScenarioError::Invalid("event_seen assertion has no event".to_string())
                    })?;
                    if lotus_trace::EventCategory::from_id(event).is_none() {
                        return Err(ScenarioError::Invalid(format!(
                            "event_seen assertion names an unknown event: {event}"
                        )));
                    }
                }
                "no_worker_crash" | "frame_count" => {}
                other => {
                    return Err(ScenarioError::Invalid(format!(
                        "unknown assertion type for version 1: {other}"
                    )))
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../specs/scenario-example.yaml")
            .to_path_buf()
    }

    fn assertion() -> Assertion {
        Assertion {
            kind: String::new(),
            event: None,
            count_at_least: None,
            minimum: None,
            maximum: None,
            equals: None,
            at_frame: None,
            address: None,
            width: None,
            core: None,
            register: None,
            text_contains: None,
            screen: None,
            sha256: None,
        }
    }

    #[test]
    fn example_scenario_loads_and_validates() {
        let scenario = Scenario::load(example_path()).expect("example scenario must load");
        assert_eq!(scenario.version, 2);
        assert_eq!(scenario.profile.worker_flavor, WorkerFlavor::Experimental);
        assert!(scenario.validate().is_ok());
    }

    #[test]
    fn unbounded_scenarios_are_rejected() {
        let mut scenario = Scenario::load(example_path()).unwrap();
        scenario.stop.max_frames = MAX_SCENARIO_FRAMES + 1;
        assert!(matches!(
            scenario.validate(),
            Err(ScenarioError::Invalid(_))
        ));
    }

    #[test]
    fn unknown_assertion_is_rejected() {
        let mut scenario = Scenario::load(example_path()).unwrap();
        scenario.assertions.push(Assertion {
            kind: "expect_magic".to_string(),
            event: None,
            ..assertion()
        });
        assert!(scenario.validate().is_err());
    }

    #[test]
    fn unknown_event_name_is_rejected() {
        let mut scenario = Scenario::load(example_path()).unwrap();
        scenario.assertions.push(Assertion {
            kind: "event_seen".to_string(),
            event: Some("gpu.not_a_real_event".to_string()),
            count_at_least: Some(1),
            ..assertion()
        });
        assert!(scenario.validate().is_err());
    }
}
