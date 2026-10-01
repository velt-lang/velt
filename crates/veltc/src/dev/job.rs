//! Windows: program versions run in a job object that kills them when the supervisor's handle
//! to it closes, i.e. when `velt dev` exits in any way (Ctrl-C, closing the console, being
//! killed). Unix gets the same from the terminal's process group.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Child;
use std::sync::OnceLock;

use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// Put `child` into the session's kill-on-close job (created on first use). Best effort: if the
/// job cannot be created or joined, the program is still stopped by the supervisor normally.
pub fn kill_with_supervisor(child: &Child) {
    static JOB: OnceLock<Option<OwnedHandle>> = OnceLock::new();
    if let Some(job) = JOB.get_or_init(create_job) {
        // SAFETY: both handles are valid for the call (the child is not yet reaped).
        unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
    }
}

fn create_job() -> Option<OwnedHandle> {
    // SAFETY: an anonymous job with default security; the handle is owned by the result.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return None;
    }
    // SAFETY: see above.
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `limits` is the structure this information class expects, with its size.
    let set = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    };
    (set != 0).then_some(job)
}
