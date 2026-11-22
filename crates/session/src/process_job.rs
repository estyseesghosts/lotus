//! Windows job ownership. Children start suspended until containment is installed.
use std::io;
use std::mem;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;

use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, TRUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{self, TH32CS_SNAPTHREAD, THREADENTRY32};
use windows_sys::Win32::System::JobObjects::{
    self, JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_JOB_TIME, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    self, PROCESS_SET_QUOTA, PROCESS_TERMINATE, THREAD_SUSPEND_RESUME,
};

#[derive(Debug)]
pub(crate) struct ProcessJob {
    handle: OwnedHandle,
}

impl ProcessJob {
    pub(crate) fn attach_and_resume(pid: u32) -> io::Result<Self> {
        Self::attach(pid, None)
    }

    pub(crate) fn attach(pid: u32, cpu_seconds: Option<u32>) -> io::Result<Self> {
        // SAFETY: Windows returns owned handles. Each successful handle is wrapped
        // immediately, and all pointers refer to correctly sized live structures.
        unsafe {
            let raw = JobObjects::CreateJobObjectW(ptr::null(), ptr::null());
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self {
                handle: OwnedHandle::from_raw_handle(raw),
            };
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if let Some(seconds) = cpu_seconds {
                limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_JOB_TIME;
                // The kernel measures aggregate job user-mode CPU time in 100 ns units.
                limits.BasicLimitInformation.PerJobUserTimeLimit = i64::from(seconds) * 10_000_000;
            }
            if JobObjects::SetInformationJobObject(
                raw,
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&limits).cast(),
                mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let process = Threading::OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return Err(io::Error::last_os_error());
            }
            let process = OwnedHandle::from_raw_handle(process);
            if JobObjects::AssignProcessToJobObject(raw, process.as_raw_handle()) == 0 {
                return Err(io::Error::last_os_error());
            }
            let snapshot = ToolHelp::CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let snapshot = OwnedHandle::from_raw_handle(snapshot);
            let mut entry: THREADENTRY32 = mem::zeroed();
            entry.dwSize = mem::size_of_val(&entry) as u32;
            let mut found = ToolHelp::Thread32First(snapshot.as_raw_handle(), &mut entry);
            while found == TRUE {
                if entry.th32OwnerProcessID == pid {
                    let thread =
                        Threading::OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if thread.is_null() {
                        return Err(io::Error::last_os_error());
                    }
                    let thread = OwnedHandle::from_raw_handle(thread);
                    if Threading::ResumeThread(thread.as_raw_handle()) == u32::MAX {
                        return Err(io::Error::last_os_error());
                    }
                    return Ok(job);
                }
                found = ToolHelp::Thread32Next(snapshot.as_raw_handle(), &mut entry);
            }
            Err(io::Error::other("suspended worker thread not found"))
        }
    }
}

impl ProcessJob {
    /// End every process in the job now; also used without the worker's session lock.
    pub(crate) fn terminate(&self) {
        // SAFETY: this live owned handle refers to our job; only its members are ended.
        unsafe {
            JobObjects::TerminateJobObject(self.handle.as_raw_handle(), 1);
        }
    }
}

impl Drop for ProcessJob {
    fn drop(&mut self) {
        // SAFETY: this live owned handle refers to our job. Termination is limited
        // to its members; closing the handle also enforces kill-on-close on panic.
        unsafe {
            JobObjects::TerminateJobObject(self.handle.as_raw_handle(), 1);
        }
    }
}
