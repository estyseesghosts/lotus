//! Worker capability advertisement.
//!
//! Capabilities are reported by the worker at handshake. They must be
//! generated from actual build flags and runtime checks, never from what the
//! protocol crate would like the worker to support.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Console model a worker emulates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleModel {
    /// Original 2011 Nintendo 3DS.
    Old3ds,
    /// New Nintendo 3DS.
    New3ds,
}

/// Graphics backend a worker was built with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderBackend {
    /// CPU based renderer.
    Software,
    /// Vulkan based renderer.
    Vulkan,
    /// OpenGL based renderer.
    Opengl,
}

/// Optional worker features.
///
/// Every field is set from a build flag or a runtime probe. A feature that was
/// not compiled in must be reported as `false`, never as "probably works".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Features {
    /// CPU debugging: breakpoints, stepping, register inspection.
    pub cpu_debug: bool,
    /// GPU draw capture and shader state inspection.
    pub gpu_draw_snapshot: bool,
    /// Firmware boot without a flashback payload.
    pub firmware_boot: bool,
    /// Worker runs without a window system.
    pub headless: bool,
}

impl Default for Features {
    fn default() -> Self {
        Self {
            cpu_debug: false,
            gpu_draw_snapshot: false,
            firmware_boot: false,
            headless: false,
        }
    }
}

/// Capabilities advertised by a worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Protocol identifier, e.g. `"1.0"`.
    pub protocol: String,
    /// Azahar revision the worker was built from.
    pub worker_sha: String,
    /// Emulated console model.
    pub model: ConsoleModel,
    /// Compiled in render backends.
    pub render_backends: Vec<RenderBackend>,
    /// Runtime feature flags.
    pub features: Features,
    /// Largest single memory read accepted.
    pub max_memory_read_bytes: u64,
    /// Largest trace held in memory.
    pub max_trace_bytes: u64,
}

impl Capabilities {
    /// Default limits used by the reference worker.
    pub const DEFAULT_MAX_MEMORY_READ_BYTES: u64 = 1024 * 1024;
    /// Default trace budget used by the reference worker.
    pub const DEFAULT_MAX_TRACE_BYTES: u64 = 512 * 1024 * 1024;

    /// Whether the worker advertises a given render backend.
    pub fn supports_backend(&self, backend: RenderBackend) -> bool {
        self.render_backends.contains(&backend)
    }

    /// Whether the worker advertises a named feature.
    pub fn supports_feature(&self, feature: &str) -> bool {
        match feature {
            "cpu_debug" => self.features.cpu_debug,
            "gpu_draw_snapshot" => self.features.gpu_draw_snapshot,
            "firmware_boot" => self.features.firmware_boot,
            "headless" => self.features.headless,
            _ => false,
        }
    }
}

/// Free-form feature map used by tooling that wants a stable, name keyed view.
pub type FeatureMap = BTreeMap<String, bool>;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Capabilities {
        Capabilities {
            protocol: crate::protocol_id(),
            worker_sha: "0".repeat(40),
            model: ConsoleModel::Old3ds,
            render_backends: vec![RenderBackend::Software],
            features: Features {
                cpu_debug: true,
                ..Features::default()
            },
            max_memory_read_bytes: Capabilities::DEFAULT_MAX_MEMORY_READ_BYTES,
            max_trace_bytes: Capabilities::DEFAULT_MAX_TRACE_BYTES,
        }
    }

    #[test]
    fn backend_lookup_is_exact() {
        let caps = sample();
        assert!(caps.supports_backend(RenderBackend::Software));
        assert!(!caps.supports_backend(RenderBackend::Vulkan));
    }

    #[test]
    fn unknown_feature_is_not_advertised() {
        assert!(!sample().supports_feature("time_travel"));
    }

    #[test]
    fn capabilities_round_trip_through_json() {
        let caps = sample();
        let json = serde_json::to_string(&caps).unwrap();
        let parsed: Capabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, caps);
    }
}
