//! Running the selected toolchain's `bin/velt` in the launcher's place.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::select::Resolved;
use crate::{ENV_LAUNCHER, ENV_SELECTED};

/// How many launchers may run one another for one command before it stops: a link or default
/// whose `bin/velt` is a launcher (another root's) would otherwise start launchers forever.
pub const MAX_HOPS: u32 = 3;
/// How many launchers ran before this one, for this command.
pub const ENV_HOPS: &str = "VELT_LAUNCHER_HOPS";

/// Run `resolved`'s `bin/velt` with `args`. On Unix the launcher becomes it (`exec`), so signals,
/// the terminal and the exit status are the toolchain's own; on Windows it waits for it and
/// exits with its code, leaving Ctrl-C to it, in a job that ends it when the launcher is killed.
pub fn exec(resolved: &Resolved, args: &[OsString], launcher: &Path) -> Result<i32, String> {
    let velt = resolved.velt();
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same(&velt, launcher) {
        return Err(format!(
            "toolchain {} ({}) is this launcher: {} runs the launcher again",
            resolved.toolchain,
            resolved.reason,
            velt.display()
        ));
    }
    let hops: u32 = std::env::var(ENV_HOPS)
        .ok()
        .and_then(|h| h.trim().parse().ok())
        .unwrap_or(0);
    if hops >= MAX_HOPS {
        return Err(format!(
            "velt launchers started one another {hops} times for this command; toolchain {} \
             ({}) at {} is a launcher, not a toolchain: link or select a toolchain prefix",
            resolved.toolchain,
            resolved.reason,
            resolved.prefix.display()
        ));
    }
    let mut command = Command::new(&velt);
    command
        .args(args)
        .env(ENV_LAUNCHER, launcher)
        .env(ENV_SELECTED, resolved.describe())
        .env(ENV_HOPS, (hops + 1).to_string());
    run(command, &velt)
}

#[cfg(unix)]
fn run(mut command: Command, velt: &Path) -> Result<i32, String> {
    use std::os::unix::process::CommandExt;
    let e = command.exec();
    Err(format!("cannot run {}: {e}", velt.display()))
}

#[cfg(windows)]
fn run(mut command: Command, velt: &Path) -> Result<i32, String> {
    use windows_sys::core::BOOL;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    /// Ctrl-C reaches every process on the console; the toolchain decides what it means, and
    /// the launcher exits when it does. (Not `SetConsoleCtrlHandler(None, TRUE)`: children
    /// would inherit ignoring it.)
    unsafe extern "system" fn ignore(_: u32) -> BOOL {
        1
    }
    // SAFETY: registers a handler that only returns.
    unsafe { SetConsoleCtrlHandler(Some(ignore), 1) };
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", velt.display()))?;
    // Held until the launcher exits: when it is killed (an editor stopping `velt lsp`, a CI
    // timeout), the handle closes and the job ends the toolchain and what it started.
    let _job = job::kill_with_launcher(&child);
    let status = child
        .wait()
        .map_err(|e| format!("cannot wait for {}: {e}", velt.display()))?;
    Ok(status.code().unwrap_or(1))
}

/// A job object that kills its processes when its last handle closes (as `velt dev` and cargo
/// do; veltc/src/dev/job.rs).
#[cfg(windows)]
mod job {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::process::Child;

    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// Put `child` into a new kill-on-close job; the returned handle keeps it alive. Best
    /// effort: without a job, the toolchain still runs (and outlives a killed launcher).
    pub fn kill_with_launcher(child: &Child) -> Option<OwnedHandle> {
        let job = create_job()?;
        // SAFETY: both handles are valid for the call (the child is not yet reaped).
        unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
        Some(job)
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
}
