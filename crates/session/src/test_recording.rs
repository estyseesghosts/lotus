//! Live test recording (lotus#95): what a session did between an explicit start and stop,
//! kept as frame-anchored facts that `lotus_runner::recording` turns into a scenario.
//!
//! Start reloads the session's content, so frame 0 is a fresh boot, as in a scenario run.
//! Every live or agent input the worker accepts is kept with the reply's `input_frame`:
//! a worker with the `input_boundary` feature applies input only at a frame boundary,
//! staging it while running (`ctr_worker/control.h` `input`, `FrameBoundary`), so replaying
//! that state while paused at that frame gives the guest the same HID samples. A mark
//! pauses, which the worker also completes at a frame boundary, records both screen hashes
//! and the requested guest memory words, then resumes a playing session.
//!
//! Anything the runner cannot replay is kept as a diagnostic rather than dropped silently:
//! analog stick states, other debugger mutations, card storage the runner will not copy,
//! and settings the runner does not apply. A guest restart (epoch change) ends the
//! recording as invalid, since its frames no longer count from the recorded start.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::native_session::{InputState, ScreenRegion};

/// Most input states kept; the runner executes at most 4,096 schedule events.
pub const MAX_INPUTS: usize = 2048;
/// Most marks in one recording.
pub const MAX_MARKS: usize = 32;
/// Most memory reads per mark.
pub const MAX_MARK_READS: usize = 16;
/// Most screen regions per mark.
pub const MAX_MARK_REGIONS: usize = 8;
/// Most interventions kept with their details; later ones are only counted.
pub const MAX_INTERVENTIONS: usize = 64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestRecording {
    /// Recording identity, unique per start.
    pub id: String,
    pub session: String,
    pub session_created_unix_ms: u64,
    /// Guest epoch of the reloaded start; a different epoch invalidates the recording.
    pub epoch: u64,
    pub started_unix_ms: u64,
    /// Approved content handle and file name. The path itself is never recorded.
    pub asset: String,
    pub content_name: String,
    /// `{bytes, sha256}` from the reload.
    pub content: serde_json::Value,
    /// Worker `{source, backend, worker_sha256, renderer}`.
    pub worker: serde_json::Value,
    /// `old3ds` or `new3ds`.
    pub model: String,
    /// `software`, `vulkan` or `opengl`, from `graphics_api`.
    pub renderer: String,
    /// `deterministic_async_operations` on and `async_fs_operations` off (docs/09, #29).
    pub determinism: bool,
    /// Worker settings that differ from the worker's startup default, other than those a
    /// scenario profile sets: `name -> {default, value}`.
    pub other_settings: BTreeMap<String, serde_json::Value>,
    /// Guest files at the start that a fresh worker root does not create (`sdmc/...`,
    /// `nand/...`, `sysdata/...`): the runner starts without them.
    pub card_storage: Vec<String>,
    /// Further such files beyond those named in `card_storage`.
    #[serde(default)]
    pub card_storage_more: u64,
    pub inputs: Vec<RecordedInput>,
    pub marks: Vec<RecordedMark>,
    pub interventions: Vec<Intervention>,
    pub interventions_dropped: u64,
    /// Inputs refused because [`MAX_INPUTS`] was reached.
    pub inputs_dropped: u64,
    /// Why the recording can no longer be exported, if it cannot.
    pub invalid: Option<String>,
    /// Frame at stop; `None` while recording.
    pub stopped_at_frame: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    Host,
    Desktop,
    Mcp,
}

impl From<crate::telemetry::Actor> for InputSource {
    fn from(actor: crate::telemetry::Actor) -> Self {
        match actor {
            crate::telemetry::Actor::Host => Self::Host,
            crate::telemetry::Actor::Desktop => Self::Desktop,
            crate::telemetry::Actor::Mcp => Self::Mcp,
        }
    }
}

/// One accepted input state, effective from the end of `frame`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedInput {
    pub frame: u64,
    pub state: InputState,
    /// `live` or `scenario`.
    pub owner: String,
    pub source: InputSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkRead {
    pub address: u32,
    /// 1, 2 or 4 bytes, little-endian.
    pub width: u8,
}

/// A screen rectangle a mark hashes by itself, so a replay can check it while other parts
/// of the screen (a clock, an animation) are left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkRegion {
    /// `top` or `bottom`.
    pub screen: String,
    #[serde(flatten)]
    pub region: ScreenRegion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionHash {
    pub screen: String,
    #[serde(flatten)]
    pub region: ScreenRegion,
    /// [`crate::native_session::Frame::region_sha256`] of the mark's capture.
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkValue {
    pub address: u32,
    pub width: u8,
    pub value: u32,
}

/// A checkpoint taken while paused at `frame`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedMark {
    pub frame: u64,
    pub label: String,
    /// SHA-256 of the native BGRA top and bottom captures at scale 1, as the runner hashes.
    pub top_sha256: String,
    pub bottom_sha256: String,
    #[serde(default)]
    pub regions: Vec<RegionHash>,
    pub memory: Vec<MarkValue>,
    pub source: InputSource,
}

/// A debugger command that changed the session while recording; replay cannot repeat it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intervention {
    /// Latest frame count the host had observed; it can trail the worker while running.
    pub observed_frame: u64,
    pub operation: String,
    pub source: InputSource,
}

impl TestRecording {
    /// Bounded view for clients: everything except the individual input states.
    #[must_use]
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "session": self.session,
            "active": self.is_active(),
            "invalid": self.invalid,
            "stopped_at_frame": self.stopped_at_frame,
            "content_name": self.content_name,
            "model": self.model,
            "renderer": self.renderer,
            "determinism": self.determinism,
            "inputs": self.inputs.len(),
            "last_input_frame": self.inputs.last().map(|input| input.frame),
            "marks": self.marks,
            "interventions": self.interventions,
            "interventions_dropped": self.interventions_dropped,
            "card_storage": self.card_storage,
            "card_storage_more": self.card_storage_more,
            "other_settings": self.other_settings,
        })
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.stopped_at_frame.is_none() && self.invalid.is_none()
    }

    /// Keep an accepted input, or invalidate the recording if the guest restarted.
    pub fn note_input(&mut self, epoch: u64, input: RecordedInput) {
        if !self.is_active() || !self.same_guest(epoch) {
            return;
        }
        if self.inputs.len() >= MAX_INPUTS {
            self.inputs_dropped += 1;
            self.invalid = Some(format!(
                "more than {MAX_INPUTS} input states; stop and export sooner"
            ));
            return;
        }
        self.inputs.push(input);
    }

    pub fn note_intervention(&mut self, epoch: u64, intervention: Intervention) {
        if !self.is_active() || !self.same_guest(epoch) {
            return;
        }
        if self.interventions.len() >= MAX_INTERVENTIONS {
            self.interventions_dropped += 1;
        } else {
            self.interventions.push(intervention);
        }
    }

    /// False, and the recording invalid, when `epoch` is not the recorded start's.
    pub fn same_guest(&mut self, epoch: u64) -> bool {
        if epoch != self.epoch && self.invalid.is_none() {
            self.invalid = Some(format!(
                "the guest restarted (epoch {} -> {epoch}); frames no longer count from the recorded start",
                self.epoch
            ));
        }
        self.invalid.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording() -> TestRecording {
        TestRecording {
            id: "r".into(),
            session: "s".into(),
            session_created_unix_ms: 1,
            epoch: 3,
            started_unix_ms: 2,
            asset: "fixture".into(),
            content_name: "input.3dsx".into(),
            content: serde_json::Value::Null,
            worker: serde_json::Value::Null,
            model: "old3ds".into(),
            renderer: "software".into(),
            determinism: true,
            other_settings: BTreeMap::new(),
            card_storage: Vec::new(),
            card_storage_more: 0,
            inputs: Vec::new(),
            marks: Vec::new(),
            interventions: Vec::new(),
            interventions_dropped: 0,
            inputs_dropped: 0,
            invalid: None,
            stopped_at_frame: None,
        }
    }

    fn input(frame: u64) -> RecordedInput {
        RecordedInput {
            frame,
            state: InputState::default(),
            owner: "live".into(),
            source: InputSource::Desktop,
        }
    }

    #[test]
    fn a_restarted_guest_invalidates_and_stops_collecting() {
        let mut recording = recording();
        recording.note_input(3, input(5));
        recording.note_input(4, input(6));
        assert_eq!(recording.inputs.len(), 1);
        assert!(recording
            .invalid
            .as_deref()
            .unwrap()
            .contains("epoch 3 -> 4"));
        recording.note_input(3, input(7));
        assert_eq!(recording.inputs.len(), 1, "no input after invalidation");
    }

    #[test]
    fn the_input_limit_invalidates_instead_of_truncating() {
        let mut recording = recording();
        for frame in 0..=MAX_INPUTS as u64 {
            recording.note_input(3, input(frame));
        }
        assert_eq!(recording.inputs.len(), MAX_INPUTS);
        assert_eq!(recording.inputs_dropped, 1);
        assert!(!recording.is_active());
    }

    #[test]
    fn interventions_past_the_limit_are_counted() {
        let mut recording = recording();
        for _ in 0..MAX_INTERVENTIONS + 2 {
            recording.note_intervention(
                3,
                Intervention {
                    observed_frame: 1,
                    operation: "write_memory".into(),
                    source: InputSource::Mcp,
                },
            );
        }
        assert_eq!(recording.interventions.len(), MAX_INTERVENTIONS);
        assert_eq!(recording.interventions_dropped, 2);
        assert!(recording.is_active());
    }
}
