//! Running the selected toolchain's `bin/velt` in the launcher's place.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::select::Resolved;
use crate::{ENV_LAUNCHER, ENV_SELECTED};

/// Run `resolved`'s `bin/velt` with `args`. On Unix the launcher becomes it (`exec`), so signals,
/// the terminal and the exit status are the toolchain's own; on Windows it waits for it and
/// exits with its code, leaving Ctrl-C to it.
pub fn exec(resolved: &Resolved, args: &[OsString], launcher: &Path) -> Result<i32, String> {
    let velt = resolved.velt();
    let mut command = Command::new(&velt);
    command
        .args(args)
        .env(ENV_LAUNCHER, launcher)
        .env(ENV_SELECTED, resolved.describe());
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
    let status = command
        .status()
        .map_err(|e| format!("cannot run {}: {e}", velt.display()))?;
    Ok(status.code().unwrap_or(1))
}
