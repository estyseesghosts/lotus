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
//!
//! Admission to a session-long tool is a [`ToolPermit`] taken before the first `await` (#44).
//! A pending enable counts against the limit, so concurrent requests on different sessions
//! cannot both pass a check made before either committed. The permit records the outcome only
//! through [`ToolPermit::commit`] after the worker accepted; dropping it (worker error, timeout,
//! cancelled task) frees the pending slot and leaves the recorded state as it was.
use std::collections::{BTreeMap, BTreeSet};
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

/// Sessions using one session-long tool, and enables still waiting for their worker.
#[derive(Debug, Default)]
struct Holders {
    active: BTreeSet<String>,
    /// Session -> enable requests in flight.
    pending: BTreeMap<String, usize>,
}

impl Holders {
    /// Sessions that consume the budget: active or with an enable in flight.
    fn occupied(&self) -> BTreeSet<&String> {
        self.active.iter().chain(self.pending.keys()).collect()
    }
}

#[derive(Debug, Default)]
struct Ledger {
    rewind: Holders,
    gpu: Holders,
    /// Sessions that ended while a permit was in flight; such a permit cannot commit.
    ended: BTreeSet<String>,
}

impl Ledger {
    fn tool(&mut self, tool: Tool) -> &mut Holders {
        match tool {
            Tool::Rewind => &mut self.rewind,
            Tool::GpuCapture => &mut self.gpu,
        }
    }

    fn has_pending(&self, session: &str) -> bool {
        self.rewind.pending.contains_key(session) || self.gpu.pending.contains_key(session)
    }
}

/// Admission to enable or disable a session-long tool; see the module docs.
#[must_use = "dropping the permit discards the admission"]
pub struct ToolPermit<'a> {
    budgets: &'a Budgets,
    tool: Tool,
    session: String,
    /// Holds a pending enable slot.
    pending: bool,
}

impl ToolPermit<'_> {
    /// The worker accepted: record whether the session now uses the tool.
    pub fn commit(mut self, active: bool) {
        let mut ledger = self.budgets.ledger();
        let ended = ledger.ended.contains(&self.session);
        self.finish(&mut ledger);
        if ended {
            return;
        }
        let holders = ledger.tool(self.tool);
        if active {
            holders.active.insert(self.session.clone());
        } else {
            holders.active.remove(&self.session);
        }
    }

    fn finish(&mut self, ledger: &mut Ledger) {
        if !std::mem::take(&mut self.pending) {
            return;
        }
        let holders = ledger.tool(self.tool);
        if let Some(count) = holders.pending.get_mut(&self.session) {
            *count -= 1;
            if *count == 0 {
                holders.pending.remove(&self.session);
            }
        }
        if !ledger.has_pending(&self.session) {
            ledger.ended.remove(&self.session);
        }
    }
}

impl Drop for ToolPermit<'_> {
    fn drop(&mut self) {
        if self.pending {
            let budgets = self.budgets;
            self.finish(&mut budgets.ledger());
        }
    }
}

/// Which sessions hold each budget, and the one-shot slots.
#[derive(Debug)]
pub struct Budgets {
    policy: Policy,
    holders: Mutex<Ledger>,
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

    fn ledger(&self) -> std::sync::MutexGuard<'_, Ledger> {
        self.holders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Admit `session` to enable (`enable`) or disable `tool`. Take this before awaiting the
    /// worker and [`ToolPermit::commit`] after it accepted. A session already using the tool may
    /// reconfigure it; a disable is never refused.
    pub fn reserve(&self, tool: Tool, session: &str, enable: bool) -> io::Result<ToolPermit<'_>> {
        let limit = match tool {
            Tool::Rewind => self.policy.rewind_limit(),
            Tool::GpuCapture => self.policy.gpu_capture_limit(),
        };
        let permit = ToolPermit {
            budgets: self,
            tool,
            session: session.to_string(),
            pending: enable,
        };
        if !enable {
            return Ok(permit);
        }
        let mut ledger = self.ledger();
        let holders = ledger.tool(tool);
        let occupied = holders.occupied();
        if let Some(limit) = limit {
            if !occupied.contains(&session.to_string()) && occupied.len() >= limit {
                let users: Vec<String> = occupied.into_iter().cloned().collect();
                let pending = holders.pending.len();
                drop(ledger);
                let mut permit = permit;
                permit.pending = false;
                self.refused
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    format!(
                        "{} budget: {} of {limit} sessions already use it ({}{}); disable it \
                         there or raise policy.{} (host config)",
                        tool.name(),
                        users.len(),
                        users.join(", "),
                        if pending > 0 {
                            ", including enables in flight"
                        } else {
                            ""
                        },
                        match tool {
                            Tool::Rewind => "rewind_sessions",
                            Tool::GpuCapture => "gpu_capture_sessions",
                        }
                    ),
                ));
            }
        }
        *holders.pending.entry(session.to_string()).or_default() += 1;
        Ok(permit)
    }

    /// A session ended: it holds nothing, and its permits still in flight cannot commit.
    pub fn release(&self, session: &str) {
        let mut ledger = self.ledger();
        ledger.rewind.active.remove(session);
        ledger.gpu.active.remove(session);
        if ledger.has_pending(session) {
            ledger.ended.insert(session.to_string());
        }
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
        let ledger = self.ledger();
        let in_use = match (&self.snapshots, self.policy.snapshot_limit()) {
            (Some(semaphore), Some(limit)) => limit - semaphore.available_permits(),
            _ => 0,
        };
        serde_json::json!({
            "workload": self.policy.workload,
            "rewind": {"limit": self.policy.rewind_limit(), "sessions": ledger.rewind.active,
                "pending": ledger.rewind.pending},
            "gpu_capture": {"limit": self.policy.gpu_capture_limit(),
                "sessions": ledger.gpu.active, "pending": ledger.gpu.pending},
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
            budgets
                .reserve(Tool::Rewind, session, true)
                .expect("within budget")
                .commit(true);
        }
        let reconfigure = budgets
            .reserve(Tool::Rewind, "a", true)
            .expect("a holder may reconfigure");
        drop(reconfigure);
        let refused = budgets
            .reserve(Tool::Rewind, "c", true)
            .err()
            .expect("third");
        assert_eq!(refused.kind(), io::ErrorKind::ResourceBusy);
        assert!(refused.to_string().contains("a, b"), "{refused}");
        drop(
            budgets
                .reserve(Tool::GpuCapture, "c", true)
                .expect("separate budget"),
        );
        budgets.release("a");
        drop(budgets.reserve(Tool::Rewind, "c", true).expect("released"));

        let batch = Budgets::new(Policy {
            workload: Workload::Batch,
            ..Policy::default()
        });
        for session in ["a", "b", "c", "d"] {
            batch
                .reserve(Tool::Rewind, session, true)
                .expect("no batch limit")
                .commit(true);
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

    fn active(budgets: &Budgets, tool: Tool) -> Vec<String> {
        budgets.ledger().tool(tool).active.iter().cloned().collect()
    }

    #[test]
    fn pending_enables_count_against_the_limit() {
        for limit in 1..=3usize {
            let budgets = Budgets::new(Policy {
                rewind_sessions: Some(limit),
                ..Policy::default()
            });
            // 8 sessions ask before any worker replied: exactly `limit` are admitted.
            let sessions: Vec<String> = (0..8).map(|i| format!("s{i}")).collect();
            let permits: Vec<_> = sessions
                .iter()
                .filter_map(|id| budgets.reserve(Tool::Rewind, id, true).ok())
                .collect();
            assert_eq!(permits.len(), limit);
            let status = budgets.status(&Memory::default());
            assert_eq!(
                status["rewind"]["pending"].as_object().unwrap().len(),
                limit
            );
            // First accepted, second rejected by its worker, the rest dropped (timeout/cancel).
            let mut permits = permits.into_iter();
            permits.next().unwrap().commit(true);
            drop(permits);
            assert_eq!(active(&budgets, Tool::Rewind), vec!["s0".to_string()]);
            let status = budgets.status(&Memory::default());
            assert!(status["rewind"]["pending"].as_object().unwrap().is_empty());
            // Freed slots are reusable; totals never exceed the limit.
            let again: Vec<_> = sessions[1..]
                .iter()
                .filter_map(|id| budgets.reserve(Tool::Rewind, id, true).ok())
                .collect();
            assert_eq!(again.len(), limit - 1);
        }
    }

    #[test]
    fn transitions_commit_only_accepted_outcomes() {
        let budgets = Budgets::new(Policy {
            gpu_capture_sessions: Some(1),
            ..Policy::default()
        });
        budgets
            .reserve(Tool::GpuCapture, "a", true)
            .expect("enable")
            .commit(true);
        // A failed disable (permit dropped) leaves the tool recorded as active.
        drop(
            budgets
                .reserve(Tool::GpuCapture, "a", false)
                .expect("disable"),
        );
        assert_eq!(active(&budgets, Tool::GpuCapture), vec!["a".to_string()]);
        assert!(budgets.reserve(Tool::GpuCapture, "b", true).is_err());
        // Capture -> counters is a disable; committed, it frees the budget.
        budgets
            .reserve(Tool::GpuCapture, "a", false)
            .expect("counters")
            .commit(false);
        let b = budgets.reserve(Tool::GpuCapture, "b", true).expect("free");
        // The session ends while its enable is in flight: the late commit is ignored.
        budgets.release("b");
        b.commit(true);
        assert!(active(&budgets, Tool::GpuCapture).is_empty());
        assert!(budgets.ledger().ended.is_empty(), "no leaked end markers");
        budgets
            .reserve(Tool::GpuCapture, "c", true)
            .expect("nothing leaked")
            .commit(true);
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
