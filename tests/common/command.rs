//! `command()`: how tests start child processes. One copy, included with `#[path]` by every
//! crate whose tests run programs (so it needs no crate of its own).
//!
//! On Windows a test run from a process without a console (a CI agent, a background shell)
//! would open a console window for every program it starts; the children get a hidden console
//! instead. In a terminal they share the terminal's console as before, so Ctrl+C still reaches
//! them. Output is captured or inherited as the caller asks, as with `Command::new`.

use std::ffi::OsStr;
use std::process::Command;

/// `Command::new(program)` for a test's child process (see the module docs).
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let cmd = Command::new(program);
    #[cfg(windows)]
    let cmd = {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetConsoleWindow() -> *mut std::ffi::c_void;
        }
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // SAFETY: takes no arguments; returns this process's console window or null.
        if unsafe { GetConsoleWindow() }.is_null() {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        cmd
    };
    cmd
}
