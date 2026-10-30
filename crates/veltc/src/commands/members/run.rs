//! Running a command on several members: one `velt <command>` per member, in its directory, at
//! most half the cores at a time (each member's build already compiles on every core), a member
//! only after the selected members it depends on. Each member's output is kept and printed in one
//! piece when it finishes, under a status line naming it; a summary ends the run. A failed member
//! doesn't stop the others (CI wants every failure of one run) unless `--fail-fast` is given.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::{mpsc, Arc, Mutex};

use vpm::workspace::{Member, Workspace};

use super::{member_deps, shown};
use crate::commands::test::member_stamp;
use crate::style::{self, Stream};

/// What a member's run printed and how it ended.
struct Outcome {
    /// The exit code, `None` when it couldn't start or was killed by a signal.
    code: Option<i32>,
    /// Its output in the order it arrived: `true` for stdout.
    output: Vec<(bool, Vec<u8>)>,
    /// Why it couldn't start.
    error: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Waiting,
    Running,
    Passed,
    Failed,
    Skipped,
}

/// Run `velt <cmd> <args>` on `selected` (in dependency order); the exit code of the first member
/// that failed, else success.
pub(super) fn run(
    cmd: &str,
    args: &[OsString],
    ws: &Workspace,
    selected: &[&Member],
    fail_fast: bool,
) -> ExitCode {
    if selected.is_empty() {
        style::status("Finished", &format!("no selected member to {cmd}"));
        return ExitCode::SUCCESS;
    }
    // Installed once here, so the members find the lock up to date and none of them writes it;
    // formatting resolves nothing, so it doesn't need the registry.
    let locked = args.iter().any(|a| a == "--locked");
    if cmd != "fmt" {
        if let Err(e) = install(ws, locked) {
            style::error(&e);
            return ExitCode::from(1);
        }
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            style::error(&format!("cannot find the velt executable: {e}"));
            return ExitCode::from(1);
        }
    };
    let jobs = std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1));
    let names: BTreeSet<&str> = selected.iter().map(|m| m.name()).collect();
    let deps: Vec<Vec<usize>> = selected
        .iter()
        .map(|m| {
            member_deps(ws, m)
                .into_iter()
                .filter(|d| names.contains(d))
                .filter_map(|d| selected.iter().position(|s| s.name() == d))
                .collect()
        })
        .collect();
    let color = style::enabled(Stream::Stderr);
    let mut states = vec![State::Waiting; selected.len()];
    // A member whose last `velt test` passed with every result kept, and of which nothing
    // changed, passes again without a `velt` of its own (`test::member_stamp`).
    if cmd == "test" {
        let mut command = vec![OsString::from(cmd)];
        command.extend(args.iter().cloned());
        for (i, m) in selected.iter().enumerate() {
            if let Some(tests) = member_stamp::up_to_date(&m.dir, &command) {
                let name = format!("{} ({})", m.name(), shown(ws, &m.dir));
                style::status("Unchanged", &format!("{name}: {tests} passed"));
                states[i] = State::Passed;
            }
        }
    }
    let mut first_failure: Option<i32> = None;
    let (tx, rx) = mpsc::channel::<(usize, Outcome)>();
    std::thread::scope(|scope| {
        let mut running = 0;
        loop {
            let stop = fail_fast && first_failure.is_some();
            while running < jobs && !stop {
                let ready = (0..selected.len()).find(|&i| {
                    states[i] == State::Waiting
                        && deps[i].iter().all(|&d| {
                            matches!(states[d], State::Passed | State::Failed | State::Skipped)
                        })
                });
                let Some(i) = ready else { break };
                states[i] = State::Running;
                running += 1;
                let (tx, exe, dir) = (tx.clone(), &exe, &selected[i].dir);
                scope.spawn(move || {
                    let outcome = run_member(exe, cmd, args, dir, color);
                    let _ = tx.send((i, outcome));
                });
            }
            if running == 0 {
                break;
            }
            let Ok((i, outcome)) = rx.recv() else { break };
            running -= 1;
            let passed = outcome.code == Some(0);
            report(ws, selected[i], &outcome, passed);
            states[i] = if passed { State::Passed } else { State::Failed };
            if !passed && first_failure.is_none() {
                first_failure = Some(outcome.code.unwrap_or(1));
            }
        }
    });
    for s in states.iter_mut().filter(|s| **s == State::Waiting) {
        *s = State::Skipped;
    }
    summary(cmd, selected, &states);
    match first_failure {
        None => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(u8::try_from(code).ok().filter(|&c| c != 0).unwrap_or(1)),
    }
}

/// Resolve and fetch the whole workspace into its lockfile.
fn install(ws: &Workspace, locked: bool) -> Result<(), String> {
    let opts = vpm::InstallOptions {
        locked,
        update: false,
        target: None,
    };
    vpm::install::workspace::install(ws, None, &vpm::Locations::from_env()?, opts).map(|_| ())
}

/// One member's run, its output collected from both streams as it arrives.
fn run_member(exe: &Path, cmd: &str, args: &[OsString], dir: &Path, color: bool) -> Outcome {
    let mut command = Command::new(exe);
    command
        .arg(cmd)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if color {
        command.env("CLICOLOR_FORCE", "1");
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Outcome {
                code: None,
                output: vec![],
                error: Some(format!("cannot start velt: {e}")),
            }
        }
    };
    let output = Arc::new(Mutex::new(Vec::new()));
    let readers = [
        child
            .stdout
            .take()
            .map(|s| (true, Box::new(s) as Box<dyn Read + Send>)),
        child
            .stderr
            .take()
            .map(|s| (false, Box::new(s) as Box<dyn Read + Send>)),
    ];
    std::thread::scope(|scope| {
        for (is_stdout, mut stream) in readers.into_iter().flatten() {
            let output = Arc::clone(&output);
            scope.spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut out) = output.lock() {
                        out.push((is_stdout, buf[..n].to_vec()));
                    }
                }
            });
        }
    });
    let code = child.wait().ok().and_then(|s| s.code());
    let output = Arc::try_unwrap(output)
        .map(|m| m.into_inner().unwrap_or_default())
        .unwrap_or_default();
    Outcome {
        code,
        output,
        error: None,
    }
}

/// A finished member: a status line naming it, then what it printed.
fn report(ws: &Workspace, m: &Member, outcome: &Outcome, passed: bool) {
    let name = format!("{} ({})", m.name(), shown(ws, &m.dir));
    if passed {
        style::status("Member", &name);
    } else {
        eprintln!(
            "{} {name}",
            style::paint(
                Stream::Stderr,
                style::Style::Error,
                &format!("{:>12}", "Failed")
            )
        );
    }
    let (mut stdout, mut stderr) = (std::io::stdout().lock(), std::io::stderr().lock());
    for (is_stdout, bytes) in &outcome.output {
        let _ = match is_stdout {
            true => stdout.write_all(bytes).and_then(|()| stdout.flush()),
            false => stderr.write_all(bytes).and_then(|()| stderr.flush()),
        };
    }
    if let Some(e) = &outcome.error {
        let _ = writeln!(stderr, "{e}");
    }
}

/// The last line: how many members passed, failed (named) and were skipped.
fn summary(cmd: &str, selected: &[&Member], states: &[State]) {
    let named = |want: State| -> Vec<&str> {
        selected
            .iter()
            .zip(states)
            .filter(|(_, s)| **s == want)
            .map(|(m, _)| m.name())
            .collect()
    };
    let (passed, failed, skipped) = (
        named(State::Passed),
        named(State::Failed),
        named(State::Skipped),
    );
    let mut line = format!(
        "`velt {cmd}` on {} members: {} passed",
        selected.len(),
        passed.len()
    );
    if !failed.is_empty() {
        line.push_str(&format!(
            ", {} failed ({})",
            failed.len(),
            failed.join(", ")
        ));
    }
    if !skipped.is_empty() {
        line.push_str(&format!(
            ", {} skipped after a failure ({})",
            skipped.len(),
            skipped.join(", ")
        ));
    }
    match failed.is_empty() {
        true => style::status("Finished", &line),
        false => style::error(&line),
    }
}
