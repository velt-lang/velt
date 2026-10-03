//! `velt dev`: the fast edit loop (docs/internals/design/hot-reload.md, phases 1–2).
//!
//! - [`supervisor`]: builds, starts the program, watches the files the build read and restarts
//!   on a change; a failed build leaves the old version running.
//! - [`host`]: `velt dev --host`, the child that compiles the program to VIR, JIT-compiles it
//!   and runs it inside this `velt` process (no link, no new executable for the OS to check).
//! - [`listeners`]: the dev channel; the supervisor owns listening sockets and hands them to every
//!   version, so a restart refuses no connection.
//! - [`versions`]: one executable per version in `--exe` mode, deleted once it has exited.
//! - [`child`], [`watch`]: process control and change detection (OS notifications, polling as
//!   the fallback), shared with `velt test --watch` ([`test_watch`]);
//! - `interrupt`: Ctrl-C, SIGTERM and friends let the supervisor stop the program and wait for
//!   it before exiting; on Windows `job` also ties programs to the supervisor.

mod child;
mod host;
mod interrupt;
#[cfg(windows)]
mod job;
mod listeners;
mod native;
mod supervisor;
mod swap;
mod versions;
mod watch;

use std::process::ExitCode;

use crate::cli::DevArgs;
use crate::commands::test::Outcome;

pub use host::host_command;

/// `velt dev` (supervisor modes). Runs until interrupted.
pub fn dev_command(args: DevArgs) -> ExitCode {
    match supervisor::Supervisor::new(args) {
        Ok(supervisor) => supervisor.run(),
        Err(msg) => {
            crate::style::error(&msg);
            ExitCode::from(1)
        }
    }
}

/// `velt test --watch`: run the tests, then again after every change to a file they read.
pub fn test_watch(mut run: impl FnMut() -> Result<Outcome, String>) -> ! {
    let mut watcher = watch::Watcher::default();
    loop {
        let snapshot = watcher.snapshot();
        match run() {
            Ok(outcome) => watcher.set(outcome.watched, &snapshot),
            // Nothing was read (e.g. a bad path); keep watching what worked before.
            Err(msg) => crate::style::error(&msg),
        }
        eprintln!("velt test: waiting for changes");
        while watcher.poll().is_none() {
            std::thread::sleep(watch::POLL);
        }
    }
}
