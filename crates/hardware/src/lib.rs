//! Hardware model vocabulary.
//!
//! This crate holds the vocabulary that the rest of the project uses to talk
//! about emulated hardware: which subsystem a claim is about, which time
//! domain a number lives in and how strong the evidence behind a claim is.
//!
//! It deliberately contains no emulation. `crates/hardware` describes the
//! contract, not an implementation.

use serde::{Deserialize, Serialize};

/// An emulated hardware subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subsystem {
    /// ARM11 (CPX) application core.
    Arm11,
    /// ARM9 (ARM946E-S) system core.
    Arm9,
    /// Kernel and HLE services.
    Kernel,
    /// PICA200 graphics unit.
    Pica200,
    /// Memory bus and caches.
    Memory,
    /// DSP / sound.
    Dsp,
    /// Display and scanout.
    Display,
    /// Event scheduler.
    Scheduler,
}

impl Subsystem {
    /// Stable identifier used in manifests and reports.
    pub fn id(&self) -> &'static str {
        match self {
            Subsystem::Arm11 => "arm11",
            Subsystem::Arm9 => "arm9",
            Subsystem::Kernel => "kernel",
            Subsystem::Pica200 => "pica200",
            Subsystem::Memory => "memory",
            Subsystem::Dsp => "dsp",
            Subsystem::Display => "display",
            Subsystem::Scheduler => "scheduler",
        }
    }
}

/// The time domain a measurement lives in.
///
/// Host time, virtual guest time and inferred hardware cycles must never be
/// mixed. Every recorded number declares its domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimingDomain {
    /// Wall clock time spent on the host.
    HostMillis,
    /// Virtual ticks executed by the emulated guest.
    GuestTicks,
    /// Cycles inferred from hardware measurements.
    EstimatedHardwareCycles,
}

impl TimingDomain {
    /// Whether numbers in this domain can be compared with the hardware.
    pub fn is_hardware_comparable(&self) -> bool {
        matches!(self, TimingDomain::EstimatedHardwareCycles)
    }
}

/// Confidence level of an accuracy claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Only documented, not verified.
    Documented,
    /// Verified on physical hardware.
    Measured,
    /// Derived from other evidence.
    Inferred,
    /// Not known.
    Unknown,
}

impl Confidence {
    /// Stable identifier used in reports.
    pub fn id(&self) -> &'static str {
        match self {
            Confidence::Documented => "documented",
            Confidence::Measured => "measured",
            Confidence::Inferred => "inferred",
            Confidence::Unknown => "unknown",
        }
    }
}

/// An accuracy claim about one subsystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccuracyClaim {
    /// Subsystem the claim is about.
    pub subsystem: Subsystem,
    /// What the claim states.
    pub statement: String,
    /// How strong the evidence is.
    pub confidence: Confidence,
    /// Reference to the evidence: a hardware capture, a document or a test.
    pub evidence: String,
}

impl AccuracyClaim {
    /// Whether the claim may be presented as a hardware result.
    pub fn is_hardware_backed(&self) -> bool {
        self.confidence == Confidence::Measured
    }
}

/// Status of a hardware model in this project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    /// Not started.
    NotStarted,
    /// Under construction, results are not comparable yet.
    InProgress,
    /// Independently validated against a reference.
    Validated,
    /// Superseded by another model.
    Retired,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_estimated_cycles_are_hardware_comparable() {
        assert!(!TimingDomain::HostMillis.is_hardware_comparable());
        assert!(!TimingDomain::GuestTicks.is_hardware_comparable());
        assert!(TimingDomain::EstimatedHardwareCycles.is_hardware_comparable());
    }

    #[test]
    fn measured_claims_are_hardware_backed() {
        let claim = AccuracyClaim {
            subsystem: Subsystem::Pica200,
            statement: "draw setup cost".to_string(),
            confidence: Confidence::Measured,
            evidence: "capture/2026-11-12".to_string(),
        };
        assert!(claim.is_hardware_backed());

        let inferred = AccuracyClaim {
            confidence: Confidence::Inferred,
            ..claim
        };
        assert!(!inferred.is_hardware_backed());
    }

    #[test]
    fn subsystem_ids_are_stable() {
        assert_eq!(Subsystem::Pica200.id(), "pica200");
        assert_eq!(Subsystem::Arm11.id(), "arm11");
    }
}
