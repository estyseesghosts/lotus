//! Unix worker ownership (#104), the counterpart of `process_job.rs`.
//!
//! Each worker leads its own process group (`setpgid(0, 0)` before exec), and descendants
//! stay in it unless they detach with `setsid`/`setpgid`. Termination is `killpg(SIGKILL)`,
//! matching `TerminateJobObject`; the graceful path is the protocol `Stop` exchange.
//!
//! PID reuse: a PGID cannot be allocated as a new PID while any member of the group
//! exists (POSIX "process group lifetime"). The group is retired before the supervisor
//! reaps the leader on the forced paths, and immediately after reaping on a clean stop,
//! when only surviving descendants can still hold the PGID. After retirement nothing
//! signals it again.
use std::sync::Mutex;

#[derive(Debug)]
pub(crate) struct ProcessGroup {
    pgid: libc::pid_t,
    live: Mutex<bool>,
}

impl ProcessGroup {
    pub(crate) fn new(pid: u32) -> Self {
        Self {
            pgid: pid as libc::pid_t,
            live: Mutex::new(true),
        }
    }

    /// SIGKILL every member now; also used without the worker's session lock.
    pub(crate) fn terminate(&self) {
        let live = self
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *live {
            self.kill();
        }
    }

    /// Kill remaining members and never signal this PGID again.
    pub(crate) fn retire(&self) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if *live {
            self.kill();
            *live = false;
        }
    }

    fn kill(&self) {
        // SAFETY: killpg only sends a signal; the PGID is ours while `live` (module docs).
        unsafe {
            libc::killpg(self.pgid, libc::SIGKILL);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.retire();
    }
}
