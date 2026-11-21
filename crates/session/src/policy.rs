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
/// The default async queue keeps the same one-serialized-checkpoint limit per worker as
/// synchronous compression (lotus#32). Larger worker queues increase that limit.
const INTERACTIVE_BUDGET: usize = 2;
/// New 3DS measured 274 MiB per serialized checkpoint (docs/34). Leave
/// headroom for guest-dependent state and apply the same estimate to both models.
const CHECKPOINT_ESTIMATE_MIB: usize = 320;
const INTERACTIVE_REWIND_MEMORY_MIB: usize = 1536;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub workload: Workload,
    /// Sessions recording rewind checkpoints at once. Unset: 2 interactive, unlimited batch.
    pub rewind_sessions: Option<usize>,
    /// Aggregate estimated checkpoint memory plus configured rewind history.
    pub rewind_memory_mib: Option<usize>,
    /// Sessions with GPU capture above `counters` at once. Unset: 2 interactive, unlimited batch.
    pub gpu_capture_sessions: Option<usize>,
    /// One-shot full-state operations at once across the host. Unset: 2 interactive,
    /// unlimited batch.
    pub snapshot_slots: Option<usize>,
    /// Longest wait for a snapshot slot before the request fails with the reason.
    pub snapshot_wait_ms: u64,
    /// Requests allowed to wait for a snapshot slot. Zero refuses a wait.
    pub snapshot_queue_limit: usize,
    /// New sessions are refused while available physical memory would drop below this.
    /// 0 disables the check.
    pub memory_reserve_mib: u64,
    /// GPU headroom admission for new Vulkan sessions ([`GpuMemory`], lotus#46).
    pub gpu_admission: bool,
    /// Device-local memory kept free beyond a session's estimated need. 0 keeps none.
    pub gpu_reserve_mib: u64,
    /// Raise the focused session's worker to the above-normal priority class (Windows).
    /// Off until `tools/bench_parallel.py --focus-priority` shows a gain on a quiet host.
    pub focus_priority: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            workload: Workload::Interactive,
            rewind_sessions: None,
            rewind_memory_mib: None,
            gpu_capture_sessions: None,
            snapshot_slots: None,
            snapshot_wait_ms: 10_000,
            snapshot_queue_limit: 16,
            memory_reserve_mib: 1024,
            gpu_admission: true,
            gpu_reserve_mib: 0,
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

    fn rewind_memory_limit(&self) -> Option<usize> {
        match (self.rewind_memory_mib, self.workload) {
            (Some(0), _) | (None, Workload::Batch) => None,
            (Some(limit), _) => Some(limit),
            (None, Workload::Interactive) => Some(INTERACTIVE_REWIND_MEMORY_MIB),
        }
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
    active: BTreeMap<String, usize>,
    /// Session -> requested weights and their in-flight counts.
    pending: BTreeMap<String, BTreeMap<usize, usize>>,
}

impl Holders {
    /// Ended sessions stay here only until their outstanding permits are dropped.
    fn live_pending<'a>(
        &'a self,
        ended: &'a BTreeSet<String>,
    ) -> impl Iterator<Item = (&'a String, &'a BTreeMap<usize, usize>)> {
        self.pending.iter().filter(|(id, _)| !ended.contains(*id))
    }

    /// Sessions that consume the budget: active or with an enable in flight.
    fn occupied<'a>(&'a self, ended: &'a BTreeSet<String>) -> BTreeMap<&'a str, usize> {
        let mut occupied: BTreeMap<_, _> = self
            .active
            .iter()
            .map(|(id, &weight)| (id.as_str(), weight))
            .collect();
        for (id, weights) in self.live_pending(ended) {
            if let Some((&weight, _)) = weights.last_key_value() {
                let reserved = occupied.entry(id.as_str()).or_default();
                *reserved = (*reserved).max(weight);
            }
        }
        occupied
    }

    fn pending_counts<'a>(&'a self, ended: &'a BTreeSet<String>) -> BTreeMap<&'a String, usize> {
        self.live_pending(ended)
            .map(|(id, weights)| (id, weights.values().sum()))
            .collect()
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
    fn holders(&self, tool: Tool) -> &Holders {
        match tool {
            Tool::Rewind => &self.rewind,
            Tool::GpuCapture => &self.gpu,
        }
    }

    fn tool(&mut self, tool: Tool) -> &mut Holders {
        match tool {
            Tool::Rewind => &mut self.rewind,
            Tool::GpuCapture => &mut self.gpu,
        }
    }

    fn has_pending(&self, session: &str) -> bool {
        self.rewind.pending.contains_key(session) || self.gpu.pending.contains_key(session)
    }

    fn clear_recordings(&mut self, session: &str) {
        self.rewind.active.remove(session);
        self.gpu.active.remove(session);
    }
}

/// Admission to enable or disable a session-long tool; see the module docs.
#[must_use = "dropping the permit discards the admission"]
pub struct ToolPermit<'a> {
    budgets: &'a Budgets,
    tool: Tool,
    session: String,
    /// Holds a pending enable slot.
    pending_weight: Option<usize>,
}

impl ToolPermit<'_> {
    /// The worker accepted: record whether the session now uses the tool.
    pub fn commit(mut self, active: bool) {
        let mut ledger = self.budgets.ledger();
        let ended = ledger.ended.contains(&self.session);
        let weight = self.pending_weight;
        self.finish(&mut ledger);
        if ended {
            return;
        }
        let holders = ledger.tool(self.tool);
        if let Some(weight) = weight.filter(|_| active) {
            holders.active.insert(self.session.clone(), weight);
        } else {
            holders.active.remove(&self.session);
        }
    }

    fn finish(&mut self, ledger: &mut Ledger) {
        let Some(weight) = self.pending_weight.take() else {
            return;
        };
        let holders = ledger.tool(self.tool);
        if let Some(weights) = holders.pending.get_mut(&self.session) {
            if let Some(count) = weights.get_mut(&weight) {
                *count -= 1;
                if *count == 0 {
                    weights.remove(&weight);
                }
            }
            if weights.is_empty() {
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
        if self.pending_weight.is_some() {
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

struct SnapshotWaiter<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for SnapshotWaiter<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
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
        self.reserve_weighted(tool, session, enable.then_some(1))
    }

    pub fn reserve_rewind(
        &self,
        session: &str,
        enable: bool,
        history_mib: u32,
        compression_queue: Option<u8>,
    ) -> io::Result<ToolPermit<'_>> {
        let buffers = compression_queue.unwrap_or(1).clamp(1, 4) as usize;
        let estimated_mib = history_mib as usize + buffers * CHECKPOINT_ESTIMATE_MIB;
        self.reserve_weighted(Tool::Rewind, session, enable.then_some(estimated_mib))
    }

    fn reserve_weighted(
        &self,
        tool: Tool,
        session: &str,
        weight: Option<usize>,
    ) -> io::Result<ToolPermit<'_>> {
        let limit = match tool {
            Tool::Rewind => self.policy.rewind_limit(),
            Tool::GpuCapture => self.policy.gpu_capture_limit(),
        };
        let mut ledger = self.ledger();
        if let Some(weight) = weight {
            let holders = ledger.holders(tool);
            let mut occupied = holders.occupied(&ledger.ended);
            if let Some(limit) =
                limit.filter(|&limit| !occupied.contains_key(session) && occupied.len() >= limit)
            {
                let users: Vec<_> = occupied.keys().copied().collect();
                let pending = holders.live_pending(&ledger.ended).next().is_some();
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
                        if pending {
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
            let reserved = occupied.entry(session).or_default();
            *reserved = (*reserved).max(weight);
            let total: usize = occupied.values().sum();
            if let Some(limit) = self
                .policy
                .rewind_memory_limit()
                .filter(|&limit| tool == Tool::Rewind && total > limit)
            {
                self.refused
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Err(io::Error::new(io::ErrorKind::ResourceBusy, format!(
                    "rewind memory budget: {total} MiB estimated with this request, limit {limit} MiB (policy.rewind_memory_mib)"
                )));
            }
            *ledger
                .tool(tool)
                .pending
                .entry(session.to_string())
                .or_default()
                .entry(weight)
                .or_default() += 1;
        }
        Ok(ToolPermit {
            budgets: self,
            tool,
            session: session.to_string(),
            pending_weight: weight,
        })
    }

    /// A session ended: it holds nothing, and its permits still in flight cannot commit.
    pub fn release(&self, session: &str) {
        let mut ledger = self.ledger();
        ledger.clear_recordings(session);
        if ledger.has_pending(session) {
            ledger.ended.insert(session.to_string());
        }
    }

    /// A reset or content load ended the recordings. Requests waiting for the
    /// worker still own their reservations until they complete or are cancelled.
    pub fn clear_recordings(&self, session: &str) {
        self.ledger().clear_recordings(session);
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
        use std::sync::atomic::Ordering::Relaxed;
        let queue_limit = self.policy.snapshot_queue_limit;
        if self
            .queued
            .fetch_update(Relaxed, Relaxed, |queued| {
                (queued < queue_limit).then(|| queued + 1)
            })
            .is_err()
        {
            self.refused.fetch_add(1, Relaxed);
            return Err(io::Error::new(
                io::ErrorKind::ResourceBusy,
                format!("{operation}: snapshot wait queue is full (policy.snapshot_queue_limit = {queue_limit})"),
            ));
        }
        let _queued = SnapshotWaiter(&self.queued);
        let wait = Duration::from_millis(self.policy.snapshot_wait_ms);
        let acquired = tokio::time::timeout(wait, semaphore.acquire()).await;
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
    pub fn status(&self, memory: &Memory, gpu: &GpuMemory) -> serde_json::Value {
        let ledger = self.ledger();
        let in_use = match (&self.snapshots, self.policy.snapshot_limit()) {
            (Some(semaphore), Some(limit)) => limit - semaphore.available_permits(),
            _ => 0,
        };
        serde_json::json!({
            "workload": self.policy.workload,
            "rewind": {"limit": self.policy.rewind_limit(), "sessions": ledger.rewind.active.keys().collect::<Vec<_>>(),
                "pending": ledger.rewind.pending_counts(&ledger.ended),
                "memory_limit_mib": self.policy.rewind_memory_limit(),
                "memory_reserved_mib": ledger.rewind.occupied(&ledger.ended)},
            "gpu_capture": {"limit": self.policy.gpu_capture_limit(),
                "sessions": ledger.gpu.active.keys().collect::<Vec<_>>(),
                "pending": ledger.gpu.pending_counts(&ledger.ended)},
            "snapshots": {"limit": self.policy.snapshot_limit(), "in_use": in_use,
                "queue_limit": self.policy.snapshot_limit().map(|_| self.policy.snapshot_queue_limit),
                "queued": self.queued.load(std::sync::atomic::Ordering::Relaxed)},
            "refused": self.refused.load(std::sync::atomic::Ordering::Relaxed),
            "focus_priority": self.policy.focus_priority,
            "memory": memory,
            "gpu_memory": gpu,
        })
    }
}

/// Host memory as seen by admission. Device memory is [`GpuMemory`].
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

/// Device-local GPU memory as seen by admission of a new Vulkan session (lotus#46), from the
/// running sessions' `status.gpu_memory` (CITnLO `ctr_worker/control.h` `GpuMemory`).
///
/// Headroom is the smallest measured value across sessions. On the largest device-local heap,
/// `VK_EXT_memory_budget` supplies a per-process headroom estimate. A supported Windows discrete
/// adapter also reports WDDM dedicated usage across all processes, giving a second estimate
/// when the Vulkan budget is unchanged by other applications. VMA's own estimate is for telemetry.
/// With no Vulkan session running there is nothing to measure and nothing is refused.
#[derive(Debug, Clone, Default, Serialize)]
pub struct GpuMemory {
    /// `VK_EXT_memory_budget`, `vma_estimate`, or null with no Vulkan session.
    pub budget_source: Option<String>,
    pub headroom_mib: Option<u64>,
    pub headroom_source: Option<&'static str>,
    /// Largest device-local high-water mark of one running session.
    pub estimate_mib: Option<u64>,
    pub reserve_mib: u64,
    /// Failed Vulkan allocations over running sessions.
    pub allocation_failures: u64,
    /// A new Vulkan session would not fit: headroom below estimate plus reserve.
    pub pressure: bool,
}

impl GpuMemory {
    /// From the `gpu_memory` objects of running sessions.
    #[must_use]
    pub fn measure<'a>(
        enabled: bool,
        reserve_mib: u64,
        sessions: impl IntoIterator<Item = &'a serde_json::Value>,
    ) -> Self {
        let mut gpu = Self {
            reserve_mib,
            ..Self::default()
        };
        for status in sessions {
            let source = status["budget_source"].as_str().unwrap_or("");
            gpu.allocation_failures += status["allocation_failures"].as_u64().unwrap_or(0);
            let extension = source == "VK_EXT_memory_budget";
            if gpu.budget_source.is_none() || extension {
                gpu.budget_source = Some(source.to_string());
            }
            let heaps: Vec<&serde_json::Value> = status["heaps"]
                .as_array()
                .map(|heaps| {
                    heaps
                        .iter()
                        .filter(|heap| heap["device_local"] == true)
                        .collect()
                })
                .unwrap_or_default();
            let high_water: u64 = heaps
                .iter()
                .filter_map(|heap| heap["block_bytes_high_water"].as_u64())
                .sum();
            gpu.estimate_mib = gpu.estimate_mib.max(Some(high_water >> 20));
            let largest = heaps
                .iter()
                .max_by_key(|heap| heap["size"].as_u64().unwrap_or(0));
            if let Some(heap) = largest {
                let vulkan = heap["budget"]
                    .as_u64()
                    .zip(heap["usage"].as_u64())
                    .filter(|_| extension)
                    .map(|(budget, usage)| (budget.saturating_sub(usage), "VK_EXT_memory_budget"));
                let wddm = heap["size"]
                    .as_u64()
                    .zip(status["adapter_dedicated_usage_bytes"].as_u64())
                    .map(|(size, usage)| (size.saturating_sub(usage), "WDDM dedicated usage"));
                if let Some((bytes, source)) = vulkan.into_iter().chain(wddm).min_by_key(|p| p.0) {
                    let headroom = bytes >> 20;
                    if gpu.headroom_mib.is_none_or(|current| headroom < current) {
                        gpu.headroom_mib = Some(headroom);
                        gpu.headroom_source = Some(source);
                    }
                }
            }
        }
        gpu.pressure = enabled
            && gpu
                .headroom_mib
                .zip(gpu.estimate_mib)
                .is_some_and(|(headroom, estimate)| {
                    headroom < estimate.saturating_add(reserve_mib)
                });
        gpu
    }

    /// Why a new Vulkan session does not fit, with the numbers; None when it does.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        self.pressure.then(|| {
            format!(
                "GPU memory: {} MiB device-local headroom ({}), a Vulkan session \
                 reached up to {} MiB and policy keeps {} MiB free (policy.gpu_reserve_mib)",
                self.headroom_mib.unwrap_or(0),
                self.headroom_source.unwrap_or("unknown"),
                self.estimate_mib.unwrap_or(0),
                self.reserve_mib
            )
        })
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
        budgets.ledger().tool(tool).active.keys().cloned().collect()
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
            let status = budgets.status(&Memory::default(), &GpuMemory::default());
            assert_eq!(
                status["rewind"]["pending"].as_object().unwrap().len(),
                limit
            );
            // First accepted, second rejected by its worker, the rest dropped (timeout/cancel).
            let mut permits = permits.into_iter();
            permits.next().unwrap().commit(true);
            drop(permits);
            assert_eq!(active(&budgets, Tool::Rewind), vec!["s0".to_string()]);
            let status = budgets.status(&Memory::default(), &GpuMemory::default());
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
        let status = budgets.status(&Memory::default(), &GpuMemory::default());
        assert!(status["gpu_capture"]["pending"]
            .as_object()
            .unwrap()
            .is_empty());
        let c = budgets
            .reserve(Tool::GpuCapture, "c", true)
            .expect("slot freed on exit");
        b.commit(true);
        assert!(active(&budgets, Tool::GpuCapture).is_empty());
        assert!(budgets.ledger().ended.is_empty(), "no leaked end markers");
        c.commit(true);
    }

    #[test]
    fn rewind_memory_counts_pending_upgrades_and_retains_active_cost_until_commit() {
        let budgets = Budgets::new(Policy {
            rewind_sessions: Some(4),
            rewind_memory_mib: Some(1300),
            ..Policy::default()
        });
        let reserve = |id, depth| budgets.reserve_rewind(id, true, 64, Some(depth));
        let status = || budgets.status(&Memory::default(), &GpuMemory::default());
        reserve("a", 0).expect("sync").commit(true);
        let upgrade = reserve("a", 3).expect("upgrade admitted");
        assert_eq!(status()["rewind"]["memory_reserved_mib"]["a"], 1024);
        assert!(reserve("b", 1).is_err(), "pending upgrade counts");
        drop(upgrade);
        assert_eq!(status()["rewind"]["memory_reserved_mib"]["a"], 384);
        reserve("b", 2)
            .expect("pending request released")
            .commit(true);
        let downgrade = reserve("b", 1).expect("downgrade admitted");
        assert!(reserve("c", 1).is_err(), "active cost counts until commit");
        downgrade.commit(true);
        reserve("c", 1).expect("downgrade applied").commit(true);
        budgets.clear_recordings("a");
        assert!(status()["rewind"]["memory_reserved_mib"].get("a").is_none());
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
        let status = budgets.status(&Memory::default(), &GpuMemory::default());
        assert_eq!(status["refused"], 1);
        assert_eq!(status["snapshots"]["in_use"], 0);
    }

    #[tokio::test]
    async fn snapshot_wait_queue_refuses_overflow_and_releases_cancelled_waiters() {
        let budgets = Budgets::new(Policy {
            snapshot_slots: Some(1),
            snapshot_queue_limit: 1,
            ..Policy::default()
        });
        let held = budgets.snapshot("save_state").await.expect("free slot");
        let mut waiting = Box::pin(budgets.snapshot("fork"));
        tokio::select! {
            biased;
            _ = &mut waiting => panic!("slot is held"),
            () = std::future::ready(()) => {}
        }
        let status = || budgets.status(&Memory::default(), &GpuMemory::default());
        assert_eq!(status()["snapshots"]["queued"], 1);
        let overflow = budgets
            .snapshot("matrix")
            .await
            .err()
            .expect("queue is full");
        assert!(
            overflow.to_string().contains("snapshot_queue_limit"),
            "{overflow}"
        );
        drop(waiting);
        assert_eq!(status()["snapshots"]["queued"], 0);
        drop(held);
        budgets.snapshot("matrix").await.expect("slot released");
    }

    fn gpu_status(
        source: &str,
        budget_mib: u64,
        usage_mib: u64,
        high_mib: u64,
    ) -> serde_json::Value {
        serde_json::json!({"budget_source": source, "allocation_failures": 1, "heaps": [
            {"device_local": false, "size": 32u64 << 30, "budget": 1u64 << 30, "usage": 0,
             "block_bytes_high_water": 1u64 << 30},
            {"device_local": true, "size": 16u64 << 30, "budget": budget_mib << 20,
             "usage": usage_mib << 20, "block_bytes_high_water": high_mib << 20}]})
    }

    #[test]
    fn gpu_admission_uses_the_smallest_extension_headroom_and_largest_high_water() {
        let a = gpu_status("VK_EXT_memory_budget", 3000, 1000, 900);
        let b = gpu_status("VK_EXT_memory_budget", 2500, 1000, 400);
        let gpu = GpuMemory::measure(true, 0, [&a, &b]);
        assert_eq!(gpu.headroom_mib, Some(1500));
        assert_eq!(gpu.estimate_mib, Some(900));
        assert_eq!(gpu.allocation_failures, 2);
        assert!(!gpu.pressure);
        let reserved = GpuMemory::measure(true, 700, [&a, &b]);
        let refusal = reserved.refusal().expect("pressure");
        assert!(
            refusal.contains("1500 MiB device-local headroom"),
            "{refusal}"
        );
        assert!(GpuMemory::measure(false, 700, [&a, &b]).refusal().is_none());
    }

    #[test]
    fn gpu_estimates_and_no_sessions_never_refuse() {
        let estimate = gpu_status("vma_estimate", 100, 100, 900);
        let gpu = GpuMemory::measure(true, 0, [&estimate]);
        assert_eq!(gpu.budget_source.as_deref(), Some("vma_estimate"));
        assert_eq!(gpu.headroom_mib, None);
        assert!(!gpu.pressure);
        let none = GpuMemory::measure(true, 0, []);
        assert!(none.budget_source.is_none() && !none.pressure);
    }

    #[test]
    fn gpu_admission_uses_adapter_usage_when_the_vulkan_budget_stays_high() {
        let mut session = gpu_status("VK_EXT_memory_budget", 16_000, 300, 500);
        assert!(!GpuMemory::measure(true, 0, [&session]).pressure);
        session["adapter_dedicated_usage_bytes"] = (16_000u64 << 20).into();
        let gpu = GpuMemory::measure(true, 0, [&session]);
        assert_eq!(gpu.headroom_mib, Some(384));
        assert_eq!(gpu.headroom_source, Some("WDDM dedicated usage"));
        assert!(gpu.refusal().unwrap().contains("WDDM dedicated usage"));
        session["adapter_dedicated_usage_bytes"] = (18_000u64 << 20).into();
        assert_eq!(
            GpuMemory::measure(true, 0, [&session]).headroom_mib,
            Some(0)
        );
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
