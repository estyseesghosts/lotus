//! Host-wide workload policy (lotus#23): interactive or batch, budgets for expensive debugger
//! work, and memory admission for new sessions.
//!
//! Normal emulation is never gated. Limits apply only to work whose cost docs/34 measured as
//! large per session: rewind checkpointing (Old 3DS 142 MiB and New 3DS 274 MiB serialized per
//! checkpoint, 18.5x Vulkan host time at every frame), GPU capture beyond counters (2.4x), and
//! one-shot full-state work (save/load state, fork, card copy, matrix runs). Session-long tools
//! over budget are refused with the sessions holding the budget; one-shot work queues for a
//! slot for a bounded time and is then refused with the reason. A refused or queued request is
//! visible in `lotus_status.policy` and the session's telemetry.
//!
//! `batch` turns every adaptive limit off unless a limit is set explicitly, so timing
//! experiments see no scheduling. Memory admission is a safety check in both modes.
use std::collections::BTreeSet;
use std::io;
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workload {
    /// Budgets on expensive debugger work so the session a person is debugging stays responsive.
    #[default]
    Interactive,
    /// No adaptive limits unless set explicitly: throughput and reproducible timing runs.
    Batch,
}

/// Interactive default for each budget. Two keeps the transient checkpoint buffers of
/// simultaneous rewind sessions near 0.3-0.6 GiB (docs/34) and never limits one session.
const INTERACTIVE_BUDGET: usize = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub workload: Workload,
    /// Sessions recording rewind checkpoints at once. Unset: 2 interactive, unlimited batch.
    pub rewind_sessions: Option<usize>,
    /// Sessions with GPU capture above `counters` at once. Unset: 2 interactive, unlimited batch.
    pub gpu_capture_sessions: Option<usize>,
    /// One-shot full-state operations at once across the host. Unset: 2 interactive,
    /// unlimited batch.
    pub snapshot_slots: Option<usize>,
    /// Longest wait for a snapshot slot before the request fails with the reason.
    pub snapshot_wait_ms: u64,
    /// New sessions are refused while available physical memory would drop below this.
    /// 0 disables the check.
    pub memory_reserve_mib: u64,
    /// Raise the focused session's worker to the above-normal priority class (Windows).
    /// Off until `tools/bench_parallel.py --focus-priority` shows a gain on a quiet host.
    pub focus_priority: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            workload: Workload::Interactive,
            rewind_sessions: None,
            gpu_capture_sessions: None,
            snapshot_slots: None,
            snapshot_wait_ms: 10_000,
            memory_reserve_mib: 1024,
            focus_priority: false,
        }
    }
}

impl Policy {
    fn budget(&self, explicit: Option<usize>) -> Option<usize> {
        match (explicit, self.workload) {
            (Some(0), _) | (None, Workload::Batch) => None,
            (Some(limit), _) => Some(limit),
            (None, Workload::Interactive) => Some(INTERACTIVE_BUDGET),
        }
    }

    #[must_use]
    pub fn rewind_limit(&self) -> Option<usize> {
        self.budget(self.rewind_sessions)
    }

    #[must_use]
    pub fn gpu_capture_limit(&self) -> Option<usize> {
        self.budget(self.gpu_capture_sessions)
    }

    #[must_use]
    pub fn snapshot_limit(&self) -> Option<usize> {
        self.budget(self.snapshot_slots)
    }
}

/// Session-long expensive tools with a host-wide budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Rewind,
    GpuCapture,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Self::Rewind => "rewind",
            Self::GpuCapture => "GPU capture",
        }
    }
}

/// Which sessions hold each budget, and the one-shot slots.
#[derive(Debug)]
pub struct Budgets {
    policy: Policy,
    holders: Mutex<(BTreeSet<String>, BTreeSet<String>)>,
    snapshots: Option<tokio::sync::Semaphore>,
    queued: std::sync::atomic::AtomicUsize,
    refused: std::sync::atomic::AtomicU64,
}

/// A held snapshot slot; released on drop.
pub struct SnapshotSlot<'a> {
    _permit: Option<tokio::sync::SemaphorePermit<'a>>,
    pub waited: Duration,
}

impl Budgets {
    #[must_use]
    pub fn new(policy: Policy) -> Self {
        Self {
            snapshots: policy.snapshot_limit().map(tokio::sync::Semaphore::new),
            policy,
            holders: Mutex::default(),
            queued: std::sync::atomic::AtomicUsize::new(0),
            refused: std::sync::atomic::AtomicU64::new(0),
        }
    }

    #[must_use]
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    fn holders(&self) -> std::sync::MutexGuard<'_, (BTreeSet<String>, BTreeSet<String>)> {
        self.holders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Check that `session` may enable `tool`. Call [`Self::set`] after the worker accepted.
    pub fn admit(&self, tool: Tool, session: &str) -> io::Result<()> {
        let limit = match tool {
            Tool::Rewind => self.policy.rewind_limit(),
            Tool::GpuCapture => self.policy.gpu_capture_limit(),
        };
        let Some(limit) = limit else { return Ok(()) };
        let holders = self.holders();
        let set = match tool {
            Tool::Rewind => &holders.0,
            Tool::GpuCapture => &holders.1,
        };
        if set.contains(session) || set.len() < limit {
            return Ok(());
        }
        self.refused
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Err(io::Error::new(
            io::ErrorKind::ResourceBusy,
            format!(
                "{} budget: {} of {limit} sessions already use it ({}); disable it there or raise \
                 policy.{} (host config)",
                tool.name(),
                set.len(),
                set.iter().cloned().collect::<Vec<_>>().join(", "),
                match tool {
                    Tool::Rewind => "rewind_sessions",
                    Tool::GpuCapture => "gpu_capture_sessions",
                }
            ),
        ))
    }

    /// Record whether `session` now uses `tool`.
    pub fn set(&self, tool: Tool, session: &str, active: bool) {
        let mut holders = self.holders();
        let set = match tool {
            Tool::Rewind => &mut holders.0,
            Tool::GpuCapture => &mut holders.1,
        };
        if active {
            set.insert(session.to_string());
        } else {
            set.remove(session);
        }
    }

    /// A session ended: it holds nothing.
    pub fn release(&self, session: &str) {
        let mut holders = self.holders();
        holders.0.remove(session);
        holders.1.remove(session);
    }

    /// Wait for a one-shot snapshot slot, at most `snapshot_wait_ms`.
    pub async fn snapshot(&self, operation: &str) -> io::Result<SnapshotSlot<'_>> {
        let Some(semaphore) = &self.snapshots else {
            return Ok(SnapshotSlot {
                _permit: None,
                waited: Duration::ZERO,
            });
        };
        let started = std::time::Instant::now();
        if let Ok(permit) = semaphore.try_acquire() {
            return Ok(SnapshotSlot {
                _permit: Some(permit),
                waited: Duration::ZERO,
            });
        }
        self.queued
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let wait = Duration::from_millis(self.policy.snapshot_wait_ms);
        let acquired = tokio::time::timeout(wait, semaphore.acquire()).await;
        self.queued
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        match acquired {
            Ok(Ok(permit)) => Ok(SnapshotSlot {
                _permit: Some(permit),
                waited: started.elapsed(),
            }),
            _ => {
                self.refused
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    format!(
                        "{operation}: all {} snapshot slots stayed busy for {} ms (policy.snapshot_slots)",
                        self.policy.snapshot_limit().unwrap_or(0),
                        wait.as_millis()
                    ),
                ))
            }
        }
    }

    /// Policy, holders and queue for `lotus_status`.
    #[must_use]
    pub fn status(&self, memory: &Memory) -> serde_json::Value {
        let holders = self.holders();
        let in_use = match (&self.snapshots, self.policy.snapshot_limit()) {
            (Some(semaphore), Some(limit)) => limit - semaphore.available_permits(),
            _ => 0,
        };
        serde_json::json!({
            "workload": self.policy.workload,
            "rewind": {"limit": self.policy.rewind_limit(), "sessions": holders.0},
            "gpu_capture": {"limit": self.policy.gpu_capture_limit(), "sessions": holders.1},
            "snapshots": {"limit": self.policy.snapshot_limit(), "in_use": in_use,
                "queued": self.queued.load(std::sync::atomic::Ordering::Relaxed)},
            "refused": self.refused.load(std::sync::atomic::Ordering::Relaxed),
            "focus_priority": self.policy.focus_priority,
            "memory": memory,
        })
    }
}

/// Host memory as seen by admission. VRAM is not measured.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Memory {
    pub available_mib: Option<u64>,
    pub reserve_mib: u64,
    /// Expected need of one more session: the largest current worker working set, or
    /// [`DEFAULT_SESSION_MIB`] with none running.
    pub estimate_mib: u64,
    /// A new session would leave less than the reserve.
    pub pressure: bool,
}

/// Working set of one t03 Vulkan CITnLO worker (962 MiB, runs/bp-real-02), rounded up.
pub const DEFAULT_SESSION_MIB: u64 = 1024;

impl Memory {
    #[must_use]
    pub fn measure(reserve_mib: u64, largest_worker_bytes: Option<u64>) -> Self {
        let estimate_mib = largest_worker_bytes.map_or(DEFAULT_SESSION_MIB, |bytes| bytes >> 20);
        let available_mib = available_bytes().map(|bytes| bytes >> 20);
        Self {
            available_mib,
            reserve_mib,
            estimate_mib,
            pressure: reserve_mib > 0
                && available_mib.is_some_and(|available| available < reserve_mib + estimate_mib),
        }
    }

    /// Refuse a new session under pressure, with the numbers.
    pub fn admit(&self) -> io::Result<()> {
        if !self.pressure {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::OutOfMemory,
            format!(
                "host memory: {} MiB available, a session needs about {} MiB and policy keeps {} \
                 MiB free (policy.memory_reserve_mib)",
                self.available_mib.unwrap_or(0),
                self.estimate_mib,
                self.reserve_mib
            ),
        ))
    }
}

/// Put a worker in the above-normal (`raised`) or normal priority class.
#[cfg(windows)]
pub fn set_priority(pid: u32, raised: bool) -> io::Result<()> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, SetPriorityClass, ABOVE_NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
        PROCESS_SET_INFORMATION,
    };
    // SAFETY: plain Win32 calls; the handle is checked and closed before returning.
    unsafe {
        let process = OpenProcess(PROCESS_SET_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let class = if raised {
            ABOVE_NORMAL_PRIORITY_CLASS
        } else {
            NORMAL_PRIORITY_CLASS
        };
        let result = if SetPriorityClass(process, class) == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        };
        CloseHandle(process);
        result
    }
}

#[cfg(not(windows))]
pub fn set_priority(_pid: u32, _raised: bool) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn available_bytes() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is a correctly sized, writable MEMORYSTATUSEX with dwLength set.
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullAvailPhys)
}

#[cfg(not(windows))]
fn available_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactive_budgets_refuse_with_holders_and_batch_has_none() {
        let budgets = Budgets::new(Policy::default());
        for session in ["a", "b"] {
            budgets.admit(Tool::Rewind, session).expect("within budget");
            budgets.set(Tool::Rewind, session, true);
        }
        budgets
            .admit(Tool::Rewind, "a")
            .expect("a holder may reconfigure");
        let refused = budgets.admit(Tool::Rewind, "c").expect_err("third");
        assert_eq!(refused.kind(), io::ErrorKind::ResourceBusy);
        assert!(refused.to_string().contains("a, b"), "{refused}");
        budgets
            .admit(Tool::GpuCapture, "c")
            .expect("separate budget");
        budgets.release("a");
        budgets.admit(Tool::Rewind, "c").expect("released");

        let batch = Budgets::new(Policy {
            workload: Workload::Batch,
            ..Policy::default()
        });
        for session in ["a", "b", "c", "d"] {
            batch.admit(Tool::Rewind, session).expect("no batch limit");
            batch.set(Tool::Rewind, session, true);
        }
        let explicit = Policy {
            workload: Workload::Batch,
            rewind_sessions: Some(1),
            ..Policy::default()
        };
        assert_eq!(explicit.rewind_limit(), Some(1));
        assert_eq!(explicit.snapshot_limit(), None);
        let unlimited = Policy {
            rewind_sessions: Some(0),
            ..Policy::default()
        };
        assert_eq!(unlimited.rewind_limit(), None);
    }

    #[tokio::test]
    async fn snapshot_slots_queue_then_refuse_with_the_reason() {
        let budgets = Budgets::new(Policy {
            snapshot_slots: Some(1),
            snapshot_wait_ms: 50,
            ..Policy::default()
        });
        let held = budgets.snapshot("save_state").await.expect("free slot");
        assert_eq!(held.waited, Duration::ZERO);
        let refused = budgets
            .snapshot("fork")
            .await
            .err()
            .expect("slot stays busy");
        assert!(refused.to_string().contains("fork"), "{refused}");
        drop(held);
        budgets.snapshot("fork").await.expect("released");
        let status = budgets.status(&Memory::default());
        assert_eq!(status["refused"], 1);
        assert_eq!(status["snapshots"]["in_use"], 0);
    }

    #[test]
    fn memory_admission_states_the_numbers() {
        let pressured = Memory {
            available_mib: Some(1500),
            reserve_mib: 1024,
            estimate_mib: 962,
            pressure: true,
        };
        let error = pressured.admit().expect_err("pressure");
        assert!(error.to_string().contains("1500 MiB available"), "{error}");
        assert!(
            Memory::measure(0, None).admit().is_ok(),
            "reserve 0 disables"
        );
    }
}
