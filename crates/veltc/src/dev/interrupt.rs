//! Interrupts of the supervisor (Ctrl-C, `kill`, a closed terminal or console) only set a flag,
//! so the supervisor loop can stop the program gracefully, wait for it and delete the version
//! executables before it exits (`--exe` files are locked on Windows while their process runs,
//! so a later session's cleanup alone cannot delete them). A second interrupt exits at once,
//! killing the programs started (and not yet reaped) first: on Unix nothing else would end
//! them, since only a terminal's Ctrl-C reaches their process group.
//!
//! - Unix: SIGINT, SIGTERM and SIGHUP. A terminal's Ctrl-C also reaches the program (same
//!   process group); a `kill` of the supervisor alone (a process manager, a container stop)
//!   reaches the program as the supervisor's stop request.
//! - Windows: Ctrl-C, Ctrl-Break and closing the console. The program gets the same console
//!   event and stops by itself.

/// Route interrupts to [`interrupted`] (best effort).
pub use imp::install;
/// Kill process `pid` if a second interrupt exits at once (until [`untrack`]); best effort for
/// a few processes at a time (the program and a host being started). Windows: a no-op, the
/// job object ends them.
pub use imp::{track, untrack};

/// `Some(exit code)` once the supervisor was asked to exit (128 + the signal on Unix, 130 on
/// Windows).
pub fn interrupted() -> Option<i32> {
    imp::interrupted()
}

#[cfg(unix)]
mod imp {
    use std::sync::atomic::{AtomicI32, Ordering};

    /// The first signal received (0: none).
    static SIGNAL: AtomicI32 = AtomicI32::new(0);

    /// Processes to kill on a second interrupt (0: a free slot).
    static TRACKED: [AtomicI32; 4] = [const { AtomicI32::new(0) }; 4];

    pub fn track(pid: u32) {
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        for slot in &TRACKED {
            if slot
                .compare_exchange(0, pid, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return;
            }
        }
    }

    pub fn untrack(pid: u32) {
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        for slot in &TRACKED {
            let _ = slot.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
        }
    }

    pub fn install() {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: a zeroed `sigaction` is valid; the handler only touches an atomic and
            // calls `_exit`, both async-signal-safe.
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = handler as *const () as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaction(sig, &action, std::ptr::null_mut());
            }
        }
    }

    pub fn interrupted() -> Option<i32> {
        match SIGNAL.load(Ordering::Relaxed) {
            0 => None,
            sig => Some(128 + sig),
        }
    }

    extern "C" fn handler(sig: libc::c_int) {
        if SIGNAL.swap(sig, Ordering::Relaxed) != 0 {
            // Asked twice: don't wait any longer, but take the programs along.
            for slot in &TRACKED {
                let pid = slot.load(Ordering::SeqCst);
                if pid > 0 {
                    // SAFETY: `kill` is async-signal-safe; a tracked pid is an unreaped
                    // child of this process, so it cannot have been reused.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
            // SAFETY: `_exit` is async-signal-safe.
            unsafe { libc::_exit(128 + sig) };
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use windows_sys::core::BOOL;
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
    };

    static INTERRUPTED: AtomicBool = AtomicBool::new(false);

    pub fn track(_: u32) {}

    pub fn untrack(_: u32) {}

    pub fn install() {
        // SAFETY: registers a handler with the right signature for the life of the process.
        unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
    }

    pub fn interrupted() -> Option<i32> {
        INTERRUPTED.load(Ordering::Relaxed).then_some(130)
    }

    unsafe extern "system" fn handler(event: u32) -> BOOL {
        match event {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                if INTERRUPTED.swap(true, Ordering::Relaxed) {
                    std::process::exit(130);
                }
                1
            }
            CTRL_CLOSE_EVENT => {
                INTERRUPTED.store(true, Ordering::Relaxed);
                // Windows ends the process when this returns; the loop exits before that.
                std::thread::sleep(Duration::from_secs(3));
                1
            }
            _ => 0,
        }
    }
}
