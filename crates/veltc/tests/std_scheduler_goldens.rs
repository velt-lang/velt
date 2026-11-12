//! Goldens that failed only under the executor in Velt (`VELT_SCHEDULER=std`, #2649), run under
//! it: once it is the only executor every golden runs on it, and until then these keep what was
//! fixed for it fixed. Each is built with the variable and run with 1 and 4 workers; its output,
//! exit code and stderr lines must be the golden's, and a golden that checks for leaks must
//! balance its block counts with 1 worker (with more, a task ending on another worker as the
//! program ends may free its block after the counts are taken: #2847).

use std::path::{Path, PathBuf};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The goldens, and what each one checks under the executor in Velt.
const GOLDENS: &[&str] = &[
    // A fresh socket and one whose last read found bytes are not readable until a peek finds
    // input (std/net/fd_ready.velt).
    "std/net_close_wakes",
    // A promise woken on its task's stack that finished before the task took it gives its
    // reference back when the task ends (sched_run.velt `finish`): a `then` on a shared promise,
    // and a detached promise driving a combinator.
    "m3/promise_several_consumers",
    "lang/proxy_throws_settled_own",
];

/// `file` built with `VELT_SCHEDULER=std` (from its folder, so a golden finds its packages) into
/// `exe`.
fn build_std(file: &Path, exe: &Path) {
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(file)
        .arg("-o")
        .arg(exe)
        .current_dir(file.parent().unwrap())
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{}: {}",
        file.display(),
        String::from_utf8_lossy(&o.stderr)
    );
}

/// Runs `exe` from `dir` with `threads` workers and the block counters on: its exit code,
/// stdout and stderr.
fn run(exe: &Path, dir: &Path, threads: &str) -> (i32, String, String) {
    let o = command(exe)
        .current_dir(dir)
        .env("VELT_THREADS", threads)
        .env("VELT_RC_STATS", "1")
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env_remove("VELT_SCHEDULER")
        .env_remove("VELT_RESCUE")
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&o.stderr).replace("\r\n", "\n"),
    )
}

/// The block counts of an `rc stats:` line on `stderr` (allocated, freed).
fn blocks(stderr: &str) -> Option<(u64, u64)> {
    let line = stderr.lines().find(|l| l.starts_with("rc stats:"))?;
    let b = line
        .split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))?;
    let (a, f) = b.split_once('/')?;
    Some((a.parse().ok()?, f.parse().ok()?))
}

fn exe_name(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }))
}

#[test]
fn goldens_that_failed_only_under_the_executor_in_velt_pass_under_it() {
    if !cfg!(unix) {
        // No executor in Velt on Windows on main yet (#2723).
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let golden = manifest().join("../../tests/golden");
    let mut failures = Vec::new();
    for name in GOLDENS {
        let file = golden.join(format!("{name}.velt"));
        let exe = exe_name(dir.path(), &name.replace('/', "_"));
        build_std(&file, &exe);
        let src = std::fs::read_to_string(&file).unwrap();
        let no_leaks = src
            .lines()
            .take(10)
            .any(|l| l.trim() == "// check: no leaks");
        let want_out = std::fs::read_to_string(file.with_extension("out"))
            .unwrap()
            .replace("\r\n", "\n");
        let want_code = std::fs::read_to_string(file.with_extension("code"))
            .map(|c| c.trim().parse::<i32>().unwrap())
            .unwrap_or(0);
        let want_err = std::fs::read_to_string(file.with_extension("stderr")).unwrap_or_default();
        for threads in ["1", "4"] {
            let (code, out, err) = run(&exe, file.parent().unwrap(), threads);
            let missing: Vec<&str> = want_err
                .lines()
                .filter(|l| !l.is_empty() && !err.contains(l))
                .collect();
            let leaked = no_leaks && threads == "1" && blocks(&err).is_none_or(|(a, f)| a != f);
            if code != want_code || out != want_out || !missing.is_empty() || leaked {
                failures.push(format!(
                    "{name} (VELT_THREADS={threads}): exit {code} (want {want_code}){}\n\
                     --- stdout ---\n{out}--- want ---\n{want_out}--- stderr ---\n{err}",
                    if leaked { ", blocks not balanced" } else { "" }
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
