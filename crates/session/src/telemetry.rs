//! Per-session gauges computed from worker status replies.
//!
//! Every rate is guest frames or samples per **host** second, measured by the
//! supervisor between status replies. Speed compares that rate with Azahar's
//! `SCREEN_REFRESH_RATE` (`core/core_timing.h:42`). These are host-observed
//! throughput figures, never guest tick or hardware cycle measurements.
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::SessionState;

/// `BASE_CLOCK_RATE_ARM11 / 4481136` from Azahar `core/core_timing.h`.
pub const GUEST_REFRESH_HZ: f64 = 268_111_856.0 / 4_481_136.0;
/// Rate samples kept for sparklines.
pub const HISTORY: usize = 120;
/// Minimum host interval for one rate sample.
const WINDOW: Duration = Duration::from_millis(500);

/// Who last issued a mutating command to a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    #[default]
    Host,
    Desktop,
    Mcp,
}

/// Latest gauges for one session; cheap to clone for UI and MCP readers.
#[derive(Debug, Clone, Serialize)]
pub struct Telemetry {
    pub state: SessionState,
    /// Whether the supervisor keeps re-issuing bounded runs.
    pub playing: bool,
    /// Another command held the session at the last poll.
    pub busy: bool,
    pub frames: u64,
    pub epoch: u64,
    /// Guest frames per host second.
    pub fps: f64,
    /// `fps / GUEST_REFRESH_HZ`, 1.0 is full speed.
    pub speed: f64,
    pub fps_history: VecDeque<f32>,
    /// Stereo samples per host second submitted by the guest DSP.
    pub audio_rate: f64,
    pub audio_dropped: u64,
    pub input_owner: String,
    pub buttons: u32,
    pub touch_down: bool,
    pub last_error: String,
    pub backend_name: String,
    /// Effective renderer: `software`, `opengl`, `vulkan`, or empty before the profile is read.
    pub renderer: String,
    /// Why a session that preferred Vulkan runs on software; empty otherwise.
    pub renderer_fallback: String,
    pub pid: Option<u32>,
    /// Worker private working set, when the host reports it.
    pub memory_bytes: Option<u64>,
    pub uptime_ms: u64,
    pub last_actor: Actor,
    pub last_operation: String,
    #[serde(skip)]
    started: Instant,
    #[serde(skip)]
    window: Option<(Instant, u64, u64)>,
}

impl Telemetry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: SessionState::Idle,
            playing: false,
            busy: false,
            frames: 0,
            epoch: 0,
            fps: 0.0,
            speed: 0.0,
            fps_history: VecDeque::with_capacity(HISTORY),
            audio_rate: 0.0,
            audio_dropped: 0,
            input_owner: String::new(),
            buttons: 0,
            touch_down: false,
            last_error: String::new(),
            backend_name: String::new(),
            renderer: String::new(),
            renderer_fallback: String::new(),
            pid: None,
            memory_bytes: None,
            uptime_ms: 0,
            last_actor: Actor::Host,
            last_operation: String::new(),
            started: Instant::now(),
            window: None,
        }
    }

    /// Fold one status reply taken at `now`.
    pub fn observe(&mut self, status: &Value, state: SessionState, epoch: u64, now: Instant) {
        let frames = status["frames"].as_u64().unwrap_or(0);
        let samples = status["audio_samples"].as_u64().unwrap_or(0);
        self.state = state;
        self.busy = false;
        self.epoch = epoch;
        self.audio_dropped = status["audio_dropped"].as_u64().unwrap_or(0);
        self.input_owner = status["input_owner"].as_str().unwrap_or("").to_string();
        self.buttons = status["buttons"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        self.touch_down = status["touch_down"].as_bool().unwrap_or(false);
        self.last_error = status["last_error"].as_str().unwrap_or("").to_string();
        if let Some(name) = status["backend_name"].as_str() {
            self.backend_name = name.to_string();
        }
        // Settings::GraphicsAPI (common/settings.h): Software 0, OpenGL 1, Vulkan 2.
        self.renderer = match status["renderer"].as_i64() {
            Some(0) => "software",
            Some(1) => "opengl",
            Some(2) => "vulkan",
            _ => "",
        }
        .to_string();
        self.renderer_fallback = status["renderer_fallback"]
            .as_str()
            .unwrap_or("")
            .to_string();
        self.uptime_ms = u64::try_from(now.duration_since(self.started).as_millis()).unwrap_or(0);
        self.frames = frames;
        match self.window {
            // Load/reset restart the frame counter: restart the window.
            Some((_, start_frames, start_samples))
                if frames < start_frames || samples < start_samples =>
            {
                self.window = Some((now, frames, samples));
            }
            Some((start, start_frames, start_samples)) if now.duration_since(start) >= WINDOW => {
                let seconds = now.duration_since(start).as_secs_f64();
                self.fps = (frames - start_frames) as f64 / seconds;
                self.speed = self.fps / GUEST_REFRESH_HZ;
                self.audio_rate = (samples - start_samples) as f64 / seconds;
                if self.fps_history.len() == HISTORY {
                    self.fps_history.pop_front();
                }
                self.fps_history.push_back(self.fps as f32);
                self.window = Some((now, frames, samples));
            }
            Some(_) => {}
            None => self.window = Some((now, frames, samples)),
        }
        if state != SessionState::Running && !self.playing {
            self.fps = 0.0;
            self.speed = 0.0;
            self.audio_rate = 0.0;
        }
    }

    /// Record a mutating command for "who is driving this session" displays.
    pub fn record(&mut self, actor: Actor, operation: &str) {
        self.last_actor = actor;
        self.last_operation = operation.to_string();
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(frames: u64, samples: u64) -> Value {
        serde_json::json!({"frames": frames, "audio_samples": samples, "input_owner": "live"})
    }

    #[test]
    fn rates_use_host_windows_and_restart_after_reset() {
        let mut telemetry = Telemetry::new();
        let start = Instant::now();
        telemetry.observe(&status(0, 0), SessionState::Running, 2, start);
        assert_eq!(telemetry.fps, 0.0, "one sample cannot define a rate");
        telemetry.observe(
            &status(60, 32_768),
            SessionState::Running,
            2,
            start + Duration::from_secs(1),
        );
        assert!((telemetry.fps - 60.0).abs() < 1e-9);
        assert!((telemetry.speed - 60.0 / GUEST_REFRESH_HZ).abs() < 1e-9);
        assert!((telemetry.audio_rate - 32_768.0).abs() < 1e-9);
        assert_eq!(telemetry.fps_history.len(), 1);

        telemetry.observe(
            &status(0, 0),
            SessionState::Running,
            3,
            start + Duration::from_secs(2),
        );
        telemetry.observe(
            &status(30, 0),
            SessionState::Running,
            3,
            start + Duration::from_secs(3),
        );
        assert!((telemetry.fps - 30.0).abs() < 1e-9, "no negative rate");
    }

    #[test]
    fn guest_refresh_matches_azahar_constant() {
        assert!((GUEST_REFRESH_HZ - 59.8312).abs() < 1e-4);
    }

    #[test]
    fn paused_sessions_report_no_throughput() {
        let mut telemetry = Telemetry::new();
        let start = Instant::now();
        telemetry.observe(&status(0, 0), SessionState::Running, 1, start);
        telemetry.observe(
            &status(60, 0),
            SessionState::Paused,
            1,
            start + Duration::from_secs(1),
        );
        assert_eq!(telemetry.fps, 0.0);
    }
}
