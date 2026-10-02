//! Windows: `velt dev` and everything it starts (program versions, JIT hosts) run in a job
//! object of the test's, so the test can end them all and wait until they have exited before it
//! removes their directory. Killing `velt dev` alone leaves its programs to be ended by its own
//! kill-on-close job, asynchronously: until they are gone, the files they have open (the shared
//! runtime DLL in `target/velt/dev`) can't be deleted.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Child;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
};

/// How long the processes of a terminated job may take to exit.
const EXIT_LIMIT: Duration = Duration::from_secs(60);

pub struct Job(OwnedHandle);

impl Job {
    /// A job holding `child` and every process it starts from now on; they are killed when the
    /// job is dropped (also if the test process dies).
    ///
    /// `child` joins right after it started, before it can have started anything of its own
    /// (`velt dev` starts a program only after its first build).
    pub fn holding(child: &Child) -> Job {
        // SAFETY: an anonymous job with default security; the handle is owned below.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(!raw.is_null(), "cannot create a job object");
        // SAFETY: a valid handle that nothing else owns.
        let job = Job(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` is the structure this information class expects, with its size.
        let set = unsafe {
            SetInformationJobObject(
                job.handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        assert!(set != 0, "cannot configure the job object");
        // SAFETY: both handles are valid; the child has not been waited for.
        let joined = unsafe { AssignProcessToJobObject(job.handle(), child.as_raw_handle()) };
        assert!(joined != 0, "cannot put `velt dev` into the job object");
        job
    }

    fn handle(&self) -> *mut std::ffi::c_void {
        self.0.as_raw_handle()
    }

    /// Kill every process in the job and wait until all of them have exited and their process
    /// objects are gone (an exited process's executable and DLLs stay in use while anything
    /// holds a handle to it); whether that happened within [`EXIT_LIMIT`].
    pub fn end(&self) -> bool {
        // SAFETY: a valid job handle.
        unsafe { TerminateJobObject(self.handle(), 1) };
        let deadline = Instant::now() + EXIT_LIMIT;
        loop {
            let Some(pids) = self.process_ids() else {
                return false;
            };
            if pids.is_empty() {
                return true;
            }
            for pid in pids {
                // A process that is already gone can't be opened: nothing to wait for.
                // SAFETY: plain call; the handle (if any) is owned below.
                let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid as u32) };
                if raw.is_null() {
                    continue;
                }
                // SAFETY: a valid handle that nothing else owns; closing it drops the last
                // reference this test holds to the process.
                let process = unsafe { OwnedHandle::from_raw_handle(raw) };
                let left = deadline.saturating_duration_since(Instant::now());
                let ms = u32::try_from(left.as_millis()).unwrap_or(u32::MAX);
                // SAFETY: a valid handle.
                if unsafe { WaitForSingleObject(process.as_raw_handle(), ms) } != WAIT_OBJECT_0 {
                    return false;
                }
            }
        }
    }

    /// The ids of the processes in the job that haven't exited; `None` if the job can't be
    /// queried.
    fn process_ids(&self) -> Option<Vec<usize>> {
        // Room for many more processes than `velt dev` ever runs at once.
        const ROOM: usize = 64;
        #[repr(C)]
        struct List {
            info: JOBOBJECT_BASIC_PROCESS_ID_LIST,
            more: [usize; ROOM - 1],
        }
        let mut list = List {
            info: JOBOBJECT_BASIC_PROCESS_ID_LIST::default(),
            more: [0; ROOM - 1],
        };
        // SAFETY: `list` starts with the structure this information class expects, followed by
        // room for `ROOM` ids in total; its size says so.
        let ok = unsafe {
            QueryInformationJobObject(
                self.handle(),
                JobObjectBasicProcessIdList,
                (&mut list as *mut List).cast(),
                std::mem::size_of::<List>() as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return None;
        }
        let n = list.info.NumberOfProcessIdsInList as usize;
        let ids = std::iter::once(list.info.ProcessIdList[0]).chain(list.more);
        Some(ids.take(n.min(ROOM)).collect())
    }
}
