//! The supervised program: start it with the dev-mode environment, notice when it exits by
//! itself, and stop it gracefully before the next version starts.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use super::listeners::Handover;

/// How long a stopped program gets to finish in-flight requests before it is killed (the
/// runtime drains for up to 1 s after a stop request).
const STOP_GRACE: Duration = Duration::from_millis(1500);
/// Overrides [`STOP_GRACE`], in milliseconds: tests that must act within the grace set it far
/// longer than a loaded machine can delay them.
pub const STOP_GRACE_ENV: &str = "VELT_DEV_STOP_GRACE_MS";

/// [`STOP_GRACE`], or `$VELT_DEV_STOP_GRACE_MS`.
fn stop_grace() -> Duration {
    std::env::var(STOP_GRACE_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(STOP_GRACE, Duration::from_millis)
}

/// How to start one version of the program.
#[derive(Clone, Debug)]
pub struct Launch {
    /// Executable to run.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// Extra environment (`VELT_DEV_SOCKET`).
    pub env: Vec<(OsString, OsString)>,
    /// `--exe` mode: the version's output path (see `versions`), retired once it has exited.
    pub output: Option<PathBuf>,
}

/// A running version of the program.
pub struct Running {
    child: Child,
    output: Option<PathBuf>,
}

impl Drop for Running {
    /// A version given up on without [`Running::stop`] (it exits by itself) is no longer
    /// killed on a second interrupt: once it is reaped elsewhere its pid may be reused.
    fn drop(&mut self) {
        super::interrupt::untrack(self.id());
    }
}

impl Running {
    /// Start `launch` with inherited stdio. On Windows the program joins the supervisor's job, so
    /// it cannot outlive the supervisor (Unix: the terminal's process group does that).
    pub fn start(launch: &Launch) -> Result<Running, String> {
        let child = Command::new(&launch.program)
            .args(&launch.args)
            .envs(launch.env.iter().map(|(k, v)| (k, v)))
            .spawn()
            .map_err(|e| format!("cannot start `{}`: {e}", launch.program.display()))?;
        #[cfg(windows)]
        super::job::kill_with_supervisor(&child);
        super::interrupt::track(child.id());
        Ok(Running {
            child,
            output: launch.output.clone(),
        })
    }

    /// The program's process id (Windows: the key of its stop channel).
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// `Some(exit code)` once the program has exited (signals map to 128 + signal).
    pub fn exited(&mut self) -> Option<i32> {
        let code = match self.child.try_wait() {
            Ok(Some(status)) => crate::commands::exit_code(status),
            Ok(None) => return None,
            Err(_) => -1,
        };
        // Reaped: the pid may be reused from now on.
        super::interrupt::untrack(self.id());
        Some(code)
    }

    /// Stop the program: ask it to stop (SIGTERM on Unix, `stop` on its stop channel on
    /// Windows), then kill it after [`STOP_GRACE`] ([`STOP_GRACE_ENV`]). A program that cannot be asked is killed
    /// right away. Returns once the process has exited, with its output path.
    pub fn stop(mut self, handover: &Handover) -> Option<PathBuf> {
        if self.exited().is_none() {
            let asked = self.ask_to_stop(handover);
            if !(asked && self.wait_for_exit(stop_grace())) {
                let _ = self.child.kill();
            }
        }
        // Before reaping: until then the pid cannot be reused.
        super::interrupt::untrack(self.id());
        let _ = self.child.wait();
        #[cfg(windows)]
        handover.forget(self.id());
        self.output.take()
    }

    /// End a program that has not started running user code (a JIT host before `go`): no
    /// graceful stop needed.
    pub fn kill(mut self, handover: &Handover) {
        let _ = self.child.kill();
        super::interrupt::untrack(self.id());
        let _ = self.child.wait();
        #[cfg(windows)]
        handover.forget(self.id());
        #[cfg(not(windows))]
        let _ = handover;
    }

    #[cfg(unix)]
    fn ask_to_stop(&self, _: &Handover) -> bool {
        let Ok(pid) = i32::try_from(self.child.id()) else {
            return false;
        };
        // SAFETY: plain syscall on our own child's pid (not yet reaped, so not reused).
        unsafe { libc::kill(pid, libc::SIGTERM) == 0 }
    }

    #[cfg(windows)]
    fn ask_to_stop(&self, handover: &Handover) -> bool {
        handover.request_stop(self.id())
    }

    /// Wait up to `grace` for the program to exit; whether it did.
    fn wait_for_exit(&mut self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if self.exited().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        false
    }
}
