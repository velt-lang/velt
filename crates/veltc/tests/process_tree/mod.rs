//! A child process together with every process it starts, so a test can kill all of them.
//!
//! `velt run` starts the compiled program as a child of its own; killing only `velt run` when a
//! golden times out left the program running (and holding its build directory and a core) for as
//! long as it liked. On Windows the child goes into a job object that is killed on timeout and
//! closed with kill-on-close when the test is done with it; on Unix it leads a process group of
//! its own, which is signalled as a whole.

use std::io;
use std::process::{Child, Command};

/// A spawned child and its descendants (see the module docs).
pub struct ProcessTree {
    child: Child,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}

impl ProcessTree {
    /// Spawn `cmd` as the root of a tree. The child joins the job right after it starts, before
    /// `velt run` can have compiled a program to start.
    pub fn spawn(cmd: &mut Command) -> io::Result<ProcessTree> {
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(cmd, 0);
        let child = cmd.spawn()?;
        #[cfg(windows)]
        let job = windows::job_for(&child);
        Ok(ProcessTree {
            child,
            #[cfg(windows)]
            job,
        })
    }

    /// The root process.
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Kill the root and everything it started (best effort: they may have exited already).
    pub fn kill(&mut self) {
        #[cfg(windows)]
        if let Some(job) = &self.job {
            windows::terminate(job);
        }
        #[cfg(unix)]
        // SAFETY: signalling the process group the child leads (its pid is the group id).
        unsafe {
            libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.child.kill();
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::process::Child;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A kill-on-close job holding `child` (None if that fails: then only the child is killed).
    pub fn job_for(child: &Child) -> Option<OwnedHandle> {
        // SAFETY: an anonymous job with default security; the handle is owned by the result.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return None;
        }
        // SAFETY: see above.
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is the structure this information class expects, with its size; the
        // child's handle is valid (it is not reaped yet).
        let ok = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) != 0
                && AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) != 0
        };
        ok.then_some(job)
    }

    pub fn terminate(job: &OwnedHandle) {
        // SAFETY: a valid job handle.
        unsafe { TerminateJobObject(job.as_raw_handle(), 1) };
    }
}
