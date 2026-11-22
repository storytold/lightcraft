//! The kernel32 calls. The job's handle is owned by [`Job`] from the moment it is created and closed
//! once, on drop; a child's handle is borrowed from its `Child`, which keeps it open for the call.

use std::os::windows::io::AsRawHandle;
use std::process::Child;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};

use crate::{Error, Result};

pub struct Job(HANDLE);

impl Job {
    pub fn new() -> Result<Job> {
        // SAFETY: both arguments may be null (default security, no name); nothing else is read.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(Error::System("creating a job object", std::io::Error::last_os_error()));
        }
        // owned from here on, so every return below closes it
        let job = Job(handle);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // the processes a child starts stay out of the job, so they outlive it
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
        let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
            .map_err(|_| Error::System("sizing the job's limits", std::io::Error::other("too large")))?;
        // SAFETY: `job.0` is the open handle created above; `info` is a live
        // JOBOBJECT_EXTENDED_LIMIT_INFORMATION of `size` bytes, which the call only reads.
        let ok = unsafe { SetInformationJobObject(job.0, JobObjectExtendedLimitInformation, (&raw const info).cast(), size) };
        if ok == 0 {
            return Err(Error::System("setting the job to end its processes on close", std::io::Error::last_os_error()));
        }
        Ok(job)
    }

    pub fn add(&self, child: &Child) -> Result<()> {
        // SAFETY: `self.0` is the job's open handle (closed only on drop); the child's process handle
        // stays open while `child` is borrowed, and the call doesn't keep it.
        let ok = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle().cast()) };
        if ok == 0 {
            return Err(Error::System("adding the process to the job", std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from CreateJobObjectW and is closed only here, once. Closing the last
        // handle ends the processes added to the job (JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE).
        unsafe { CloseHandle(self.0) };
    }
}
