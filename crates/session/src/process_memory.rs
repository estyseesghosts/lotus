//! Worker working-set size, read through the supervisor's own child handle.
//!
//! Using the owned handle (not a PID) means a reaped and reused PID can never
//! be measured by mistake. Non-Windows hosts report nothing.

/// Current working set of the process behind `child`, in bytes.
#[cfg(windows)]
pub(crate) fn working_set(child: &tokio::process::Child) -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    let handle = child.raw_handle()?;
    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: `handle` is the live, owned process handle of `child`, borrowed for
    // this call only, and `counters` is a correctly sized writable structure.
    let ok = unsafe { K32GetProcessMemoryInfo(handle, &mut counters, counters.cb) };
    (ok != 0).then_some(counters.WorkingSetSize as u64)
}

#[cfg(not(windows))]
pub(crate) fn working_set(_child: &tokio::process::Child) -> Option<u64> {
    None
}
