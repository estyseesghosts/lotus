//! Bounded structured event capture.
//!
//! Events are produced by the worker, ordered by a worker-local sequence
//! number and consumed by the debugger, the runner and the trace viewer.
//! Capture is always bounded: a full buffer drops the oldest event and
//! counts the loss, because a silent truncation would make a trace look
//! complete when it is not.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Category of a captured event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    /// Worker lifecycle event.
    Worker,
    /// A PICA200 command list was submitted.
    GpuSubmitCmdList,
    /// A PICA200 draw call.
    GpuDraw,
    /// CPU sampling or breakpoint event.
    Cpu,
    /// Guest memory access.
    Memory,
    /// HLE service call.
    ServiceCall,
    /// Kernel IPC.
    KernelIpc,
    /// Emulated exception.
    Exception,
}

impl EventCategory {
    /// Stable identifier used in manifests and assertions.
    pub fn id(&self) -> &'static str {
        match self {
            EventCategory::Worker => "worker",
            EventCategory::GpuSubmitCmdList => "gpu.submit_cmdlist",
            EventCategory::GpuDraw => "gpu.draw",
            EventCategory::Cpu => "cpu",
            EventCategory::Memory => "memory",
            EventCategory::ServiceCall => "service_call",
            EventCategory::KernelIpc => "kernel.ipc",
            EventCategory::Exception => "exception",
        }
    }

    /// Look up a category by its stable identifier.
    pub fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "worker" => Self::Worker,
            "gpu.submit_cmdlist" => Self::GpuSubmitCmdList,
            "gpu.draw" => Self::GpuDraw,
            "cpu" => Self::Cpu,
            "memory" => Self::Memory,
            "service_call" => Self::ServiceCall,
            "kernel.ipc" => Self::KernelIpc,
            "exception" => Self::Exception,
            _ => return None,
        })
    }
}

/// Verbosity of a trace capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceLevel {
    /// Capture nothing.
    Off,
    /// Capture errors and exceptions only.
    Error,
    /// Capture warnings as well.
    Warn,
    /// Capture informational events.
    Info,
    /// Capture everything, including high frequency events.
    Verbose,
}

/// A single captured event.
///
/// `guest_tick` is a virtual emulation timestamp. It is not host time and it
/// is not a calibrated hardware timestamp; those must be reported separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRecord {
    /// Worker-local monotonic sequence number.
    pub sequence: u64,
    /// Frame the event belongs to.
    pub frame: u64,
    /// Virtual guest tick.
    pub guest_tick: u64,
    /// Event category.
    pub category: EventCategory,
    /// Category specific payload.
    pub payload: serde_json::Value,
}

impl EventRecord {
    /// Create a record with an empty payload.
    pub fn new(sequence: u64, frame: u64, guest_tick: u64, category: EventCategory) -> Self {
        Self {
            sequence,
            frame,
            guest_tick,
            category,
            payload: serde_json::Value::Null,
        }
    }
}

/// A bounded, ordered event buffer that reports its own loss.
#[derive(Debug)]
pub struct BoundedEventBuffer {
    capacity: usize,
    events: VecDeque<EventRecord>,
    dropped: u64,
    next_sequence: u64,
}

impl BoundedEventBuffer {
    /// Create a buffer holding at most `capacity` events.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            events: VecDeque::new(),
            dropped: 0,
            next_sequence: 0,
        }
    }

    /// Capacity of the buffer.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of events currently held.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Number of events dropped because the buffer was full.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Push an event, assigning it the next sequence number.
    ///
    /// Returns the event that was evicted, if any.
    pub fn push(&mut self, mut event: EventRecord) -> Option<EventRecord> {
        event.sequence = self.next_sequence;
        self.next_sequence += 1;
        if self.events.len() == self.capacity {
            let evicted = self.events.pop_front();
            self.dropped += 1;
            self.events.push_back(event);
            return evicted;
        }
        self.events.push_back(event);
        None
    }

    /// Iterate over the retained events in order.
    pub fn iter(&self) -> impl Iterator<Item = &EventRecord> {
        self.events.iter()
    }

    /// Number of events of a given category currently retained.
    pub fn count_of(&self, category: EventCategory) -> usize {
        self.events
            .iter()
            .filter(|e| e.category == category)
            .count()
    }

    /// Remove all events, keeping the drop counter.
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_sequenced_in_order() {
        let mut buffer = BoundedEventBuffer::new(8);
        buffer.push(EventRecord::new(0, 0, 0, EventCategory::Worker));
        buffer.push(EventRecord::new(0, 1, 10, EventCategory::GpuDraw));
        let sequences: Vec<u64> = buffer.iter().map(|e| e.sequence).collect();
        assert_eq!(sequences, vec![0, 1]);
    }

    #[test]
    fn overflow_drops_oldest_and_counts_loss() {
        let mut buffer = BoundedEventBuffer::new(2);
        for frame in 0..5 {
            buffer.push(EventRecord::new(0, frame, 0, EventCategory::Cpu));
        }
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.dropped(), 3);
        let frames: Vec<u64> = buffer.iter().map(|e| e.frame).collect();
        assert_eq!(frames, vec![3, 4]);
    }

    #[test]
    fn categories_round_trip_by_id() {
        for category in [
            EventCategory::Worker,
            EventCategory::GpuSubmitCmdList,
            EventCategory::GpuDraw,
            EventCategory::Cpu,
            EventCategory::Memory,
            EventCategory::ServiceCall,
            EventCategory::KernelIpc,
            EventCategory::Exception,
        ] {
            assert_eq!(EventCategory::from_id(category.id()), Some(category));
        }
        assert_eq!(EventCategory::from_id("nope"), None);
    }
}
