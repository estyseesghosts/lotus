//! Debugging types.
//!
//! These types describe what a debugger asks the worker for. They carry no
//! host pointers and no C++ internals: every structure is plain data that can
//! be validated before it is sent.

use lotus_protocol::capabilities::Capabilities;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Largest single memory read the worker accepts by default.
pub const DEFAULT_MAX_MEMORY_READ_BYTES: u64 = 1024 * 1024;

/// CPU core a debug request targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CpuCore {
    /// ARM11 application core.
    Arm11,
    /// ARM9 system core.
    Arm9,
}

/// How consistent a read of mutable emulated state has to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Consistency {
    /// Execute at the next CPU safe point.
    CpuSafePoint,
    /// Wait until the GPU is consistent with the CPU view.
    GpuConsistent,
    /// Wait until no asynchronous work is pending.
    FullCheckpoint,
    /// Best effort, possibly stale. The age of the data is reported.
    Telemetry,
}

/// Address space a memory request refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AddressSpace {
    /// ARM11 main RAM.
    MainRam,
    /// ARM9 private RAM.
    Iram,
    /// Video memory.
    Vram,
    /// Shared physical memory.
    SharedPage,
}

/// What a breakpoint reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakpointKind {
    /// Break when the PC reaches the address.
    Execution,
    /// Break on a read of the address.
    Read,
    /// Break on a write of the address.
    Write,
}

/// A breakpoint request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breakpoint {
    /// Guest address.
    pub address: u64,
    /// Core the breakpoint is installed on.
    pub core: CpuCore,
    /// What the breakpoint reacts to.
    pub kind: BreakpointKind,
    /// Whether the breakpoint is active.
    pub enabled: bool,
}

impl Breakpoint {
    /// A disabled execution breakpoint.
    pub fn execution(address: u64, core: CpuCore) -> Self {
        Self {
            address,
            core,
            kind: BreakpointKind::Execution,
            enabled: true,
        }
    }
}

/// A memory read request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryReadRequest {
    /// Address space to read from.
    pub space: AddressSpace,
    /// Guest process id, or `None` for a system wide view.
    pub process_id: Option<u32>,
    /// Guest address.
    pub address: u64,
    /// Number of bytes to read.
    pub length: u32,
    /// Required consistency.
    pub consistency: Consistency,
}

/// Errors raised while validating a debug request.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DebugError {
    /// The read is longer than the worker accepts.
    #[error("memory read of {length} bytes exceeds the limit of {max} bytes")]
    ReadTooLarge {
        /// Requested length.
        length: u32,
        /// Accepted maximum.
        max: u64,
    },
    /// The address plus length overflows the guest address space.
    #[error("memory range {address:#x}+{length} overflows the guest address space")]
    RangeOverflow {
        /// Start address.
        address: u64,
        /// Length.
        length: u32,
    },
    /// The worker does not advertise the required feature.
    #[error("worker does not support the required feature: {0}")]
    UnsupportedFeature(String),
}

impl MemoryReadRequest {
    /// Validate the request against the worker's advertised limits.
    pub fn validate(&self, caps: &Capabilities) -> Result<(), DebugError> {
        let max = caps
            .max_memory_read_bytes
            .min(DEFAULT_MAX_MEMORY_READ_BYTES);
        if self.length as u64 > max {
            return Err(DebugError::ReadTooLarge {
                length: self.length,
                max,
            });
        }
        let end = (self.address as u128) + (self.length as u128);
        if end > u32::MAX as u128 {
            return Err(DebugError::RangeOverflow {
                address: self.address,
                length: self.length,
            });
        }
        Ok(())
    }
}

/// ARM11 register snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arm11Registers {
    /// Program counter.
    pub r15: u32,
    /// Stack pointer.
    pub r13: u32,
    /// Link register.
    pub r14: u32,
    /// General purpose registers r0..r12.
    pub r0_12: [u32; 13],
    /// Current program status register.
    pub cpsr: u32,
}

/// A snapshot of debugger visible state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Frame the snapshot belongs to.
    pub frame: u64,
    /// Virtual guest tick.
    pub guest_tick: u64,
    /// ARM11 registers.
    pub arm11: Arm11Registers,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lotus_protocol::capabilities::{ConsoleModel, Features, RenderBackend};

    fn caps(max_read: u64) -> Capabilities {
        Capabilities {
            protocol: lotus_protocol::protocol_id(),
            worker_sha: "0".repeat(40),
            model: ConsoleModel::Old3ds,
            render_backends: vec![RenderBackend::Software],
            features: Features::default(),
            max_memory_read_bytes: max_read,
            max_trace_bytes: Capabilities::DEFAULT_MAX_TRACE_BYTES,
        }
    }

    #[test]
    fn oversized_read_is_rejected() {
        let request = MemoryReadRequest {
            space: AddressSpace::MainRam,
            process_id: None,
            address: 0,
            length: 4096,
            consistency: Consistency::CpuSafePoint,
        };
        assert!(matches!(
            request.validate(&caps(1024)),
            Err(DebugError::ReadTooLarge { .. })
        ));
    }

    #[test]
    fn overflowing_range_is_rejected() {
        let request = MemoryReadRequest {
            space: AddressSpace::MainRam,
            process_id: None,
            address: u32::MAX as u64,
            length: 16,
            consistency: Consistency::CpuSafePoint,
        };
        assert!(matches!(
            request.validate(&caps(1024 * 1024)),
            Err(DebugError::RangeOverflow { .. })
        ));
    }

    #[test]
    fn valid_read_is_accepted() {
        let request = MemoryReadRequest {
            space: AddressSpace::MainRam,
            process_id: Some(3),
            address: 0x1000,
            length: 256,
            consistency: Consistency::Telemetry,
        };
        assert!(request.validate(&caps(1024 * 1024)).is_ok());
    }
}

pub mod lines;
pub mod symbols;
pub mod variables;
