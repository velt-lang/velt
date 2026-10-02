//! Runs a child process with a wall-clock timeout, capturing stdout, stderr and the exit status.
//!
//! Output goes to files, not pipes: Node writes to pipes asynchronously on macOS, so a program
//! that ends with `process.exit` (the `panic` shim) could lose buffered output; writes to files are
//! synchronous on every platform. Files also mean a chatty child never blocks on a full pipe. A
//! child that outlives its budget is killed (generated programs may loop).

use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How a child process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Normal exit with this code.
    Exit(i32),
    /// Killed by a signal (crash) — Unix only; carries no code.
    Signal,
    /// Still running when the budget ran out; it was killed.
    Timeout,
}

/// Captured result of one process run.
#[derive(Clone, Debug)]
pub struct Output {
    /// Everything written to stdout (lossy UTF-8, CRLF normalized).
    pub stdout: String,
    /// Everything written to stderr (lossy UTF-8, CRLF normalized).
    pub stderr: String,
    /// How the process ended.
    pub status: Status,
}

/// Runs `cmd` to completion or until `timeout` elapses; `scratch` is a private directory where the
/// capture files live (overwritten on every call).
pub fn run(cmd: &mut Command, scratch: &Path, timeout: Duration) -> Result<Output, String> {
    let out_path = scratch.join("stdout.txt");
    let err_path = scratch.join("stderr.txt");
    let create =
        |p: &Path| File::create(p).map_err(|e| format!("cannot create {}: {e}", p.display()));
    // Programs and their Node twins see the same local time zone: UTC, except on Windows, where
    // Velt reads the system zone and ignores `TZ` (Node honors it), so neither side gets it.
    if cfg!(windows) {
        cmd.env_remove("TZ");
    } else {
        cmd.env("TZ", "UTC");
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(create(&out_path)?)
        .stderr(create(&err_path)?)
        .spawn()
        .map_err(|e| format!("cannot start {:?}: {e}", cmd.get_program()))?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code().map_or(Status::Signal, Status::Exit),
            Ok(None) if start.elapsed() >= timeout => {
                // Best effort: the child may have exited between the poll and the kill.
                let _ = child.kill();
                let _ = child.wait();
                break Status::Timeout;
            }
            Ok(None) => thread::sleep(Duration::from_millis(2)),
            Err(e) => return Err(format!("waiting for child failed: {e}")),
        }
    };
    Ok(Output {
        stdout: read_lossy(&out_path),
        stderr: read_lossy(&err_path),
        status,
    })
}

fn read_lossy(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_default();
    String::from_utf8_lossy(&bytes).replace("\r\n", "\n")
}
