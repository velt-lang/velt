//! Windows: Ctrl-C, Ctrl-Break or closing the console only sets a flag, so the supervisor loop
//! can stop the program and delete the version executables before it exits (`--exe` files are
//! locked while their process runs, so they cannot be deleted by a later session's cleanup
//! alone). The program gets the same console event and stops by itself.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use windows_sys::core::BOOL;
use windows_sys::Win32::System::Console::{
    SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Route console interrupts to [`interrupted`] (best effort).
pub fn install() {
    // SAFETY: registers a handler with the right signature for the life of the process.
    unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
}

/// Whether the user asked `velt dev` to exit.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::Relaxed)
}

unsafe extern "system" fn handler(event: u32) -> BOOL {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            INTERRUPTED.store(true, Ordering::Relaxed);
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
