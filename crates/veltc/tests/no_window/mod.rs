//! Child processes of tests start without a console window of their own.
//!
//! A test run from a process without a console (a CI agent, a background shell) would otherwise
//! open a window for every program it starts on Windows. Output stays captured as before.

use std::ffi::OsStr;
use std::process::Command;

/// `Command::new(program)`, with `CREATE_NO_WINDOW` on Windows.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
