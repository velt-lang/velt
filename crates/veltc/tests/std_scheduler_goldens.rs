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
    // The throw location of a promise claimed on another worker (sched_node_loc.velt).
    "lang/uncaught_location_other_worker",
    // `console.log` of a settled promise prints its value (sched_peek.velt).
    "lang/print_promise",
    "lang/print_limits",
    // A promise woken on its task's stack that finished before the task took it gives its
    // reference back when the task ends (sched_run.velt `finish`): a `then` on a shared promise,
    // and a detached promise driving a combinator.
    "m3/promise_several_consumers",
    "lang/proxy_throws_settled_own",
    "lang/promise_shared",
    // http-server's readable wait with a signal (a `within` read twice).
    "std/http_server/serve_counts_hits",
    // An uncaught rejection ends the program while the executor holds blocks the runtime's
    // executor allocates outside the counters: they are given back first (sched_exit.velt). The
    // task whose dropped result reports, resumed by its sleep (whose reference goes before the
    // poll: sched_run.velt `fireDue`), with the root of `main` idle on a sleep; and the root of
    // `main` reporting in its own poll, as it drops a finished task's handle.
    "lang/spawn_handle_dropped_rejects",
    "lang/spawn_handle_dropped_after_reject",
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

/// A program that runs nothing on the executor and prints a value whose type may be a promise:
/// `console.log` peeks at it as at a promise.
const NO_TASKS_PEEK: &str = r#"function run(n: number, f: (n: number) => number | Promise<number>): number | Promise<number> {
  return f(n);
}

function main() {
  console.log(run(3, (n): number | Promise<number> => n * 2));
}
"#;

/// `console.log` of a maybe-promise in a program sema found running nothing on the executor
/// keeps the runtime's peek (`velt_rt_fut_peek`): the executor's `sched_peek` isn't loaded there
/// (#2880; its call was an internal compiler error once every program used the executor), with
/// std on demand and with std up front (`VELT_EAGER_STD=1`, its hooks loaded but unchecked).
#[test]
fn a_maybe_promise_printed_by_a_program_without_tasks_builds_and_prints() {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("no_tasks_peek.velt");
    std::fs::write(&file, NO_TASKS_PEEK).unwrap();
    // The loader takes `VELT_EAGER_STD` set to anything for std up front.
    for eager in [false, true] {
        let exe = exe_name(dir.path(), &format!("no_tasks_peek_{eager}"));
        let mut build = command(env!("CARGO_BIN_EXE_velt"));
        build
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .current_dir(dir.path())
            .env("VELT_SCHEDULER", "std")
            .env_remove("VELT_EAGER_STD");
        if eager {
            build.env("VELT_EAGER_STD", "1");
        }
        let o = build.output().unwrap();
        assert!(
            o.status.success(),
            "std up front: {eager}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let (code, out, err) = run(&exe, dir.path(), "1");
        assert_eq!(
            (code, out.as_str()),
            (0, "6\n"),
            "std up front: {eager}: {err}"
        );
    }
}

/// The root of `main`, resumed from its microtask queue (its `await` queued behind a callback),
/// drops the handle of a task that rejected: the report ends the program in that poll. The
/// queue's reference to the root goes before the poll (sched_run.velt `drain`), so the exit gives
/// back every reference to it and the block counts balance (they were one block short).
const MICRO_EXIT: &str = r#"class Failed extends Error {}

async function failing(): Promise<i64> {
  throw new Failed("rejected");
}

async function main() {
  {
    const task = spawn(failing());
    await sleep(10);
    queueMicrotask(() => console.log("a microtask first"));
    await Promise.resolve(0);
    console.log("dropping the handle");
  }
  await sleep(60000);
  console.log("unreachable");
}
"#;

#[test]
fn an_uncaught_exit_in_a_poll_a_microtask_resumed_balances_the_block_counts() {
    if !cfg!(unix) {
        // No executor in Velt on Windows on main yet (#2723).
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("micro_exit.velt");
    std::fs::write(&file, MICRO_EXIT).unwrap();
    let exe = exe_name(dir.path(), "micro_exit");
    build_std(&file, &exe);
    for threads in ["1", "4"] {
        let (code, out, err) = run(&exe, dir.path(), threads);
        assert_eq!(code, 1, "VELT_THREADS={threads}: {err}");
        assert_eq!(
            out, "a microtask first\ndropping the handle\n",
            "VELT_THREADS={threads}"
        );
        assert!(err.contains("Uncaught Failed [Error]: rejected"), "{err}");
        // Counted with 1 worker, as the goldens' (module docs).
        if threads == "1" {
            let (a, f) = blocks(&err).expect("an rc stats line");
            assert_eq!(a, f, "blocks not balanced: {err}");
        }
    }
}

/// A program that runs nothing on the executor (no async code, no timer) and ends on an error
/// leaving a module state initializer.
const NO_TASKS_EXIT: &str = r#"class Boom extends Error {}

function make(): number {
  throw new Boom("in a module initializer");
}

const x: number = make();

function main() {
  console.log(x);
}
"#;

/// The uncaught exit of a program sema found running nothing on the executor calls the runtime's
/// `velt_rt_exit`, not the executor's `sched_exit`, whose module isn't loaded then (#2880: its
/// call was an internal compiler error), with std on demand and with std up front
/// (`VELT_EAGER_STD=1`, where the module is loaded but its hooks left unchecked).
#[test]
fn an_uncaught_exit_of_a_program_without_tasks_builds_and_ends_with_1() {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("no_tasks_exit.velt");
    std::fs::write(&file, NO_TASKS_EXIT).unwrap();
    // The loader takes `VELT_EAGER_STD` set to anything for std up front.
    for eager in [false, true] {
        let exe = exe_name(dir.path(), &format!("no_tasks_exit_{eager}"));
        let mut build = command(env!("CARGO_BIN_EXE_velt"));
        build
            .arg("build")
            .arg(&file)
            .arg("-o")
            .arg(&exe)
            .current_dir(dir.path())
            .env("VELT_SCHEDULER", "std")
            .env_remove("VELT_EAGER_STD");
        if eager {
            build.env("VELT_EAGER_STD", "1");
        }
        let o = build.output().unwrap();
        assert!(
            o.status.success(),
            "std up front: {eager}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let (code, out, err) = run(&exe, dir.path(), "1");
        assert_eq!(
            (code, out.as_str()),
            (1, ""),
            "std up front: {eager}: {err}"
        );
        assert!(
            err.contains("Uncaught Boom [Error]: in a module initializer"),
            "{err}"
        );
        let (a, f) = blocks(&err).expect("an rc stats line");
        assert_eq!(a, f, "blocks not balanced: {err}");
    }
}

/// #2962: a promise `main` held rejects, and is reported when it drops at the end of `main`,
/// after the poll's microtasks (sched_node_report.velt `report`): the exit gives back the node
/// being reported (the unhandled list's reference) and the root `main` left before `finish` ran.
const HELD_EXIT: &str = r#"async function fail(): Promise<void> {
  throw new Error("boom");
}
async function main() {
  const p = fail();
  await Promise.resolve();
  await Promise.resolve();
  console.log("main ends, dropping p");
}
"#;

/// #2962: a promise dropped in a block rejects, and is reported after the poll while `main` waits
/// in a sleep: the exit drops `main`'s suspended frame (its sleep leaves the timers) and gives back
/// the node being reported.
const SUSPENDED_EXIT: &str = r#"async function fail(): Promise<void> {
  throw new Error("boom");
}
async function main() {
  {
    const p = fail();
    console.log("dropping p");
  }
  await sleep(10);
  console.log("never printed");
}
"#;

/// Builds `src` as `name` and runs it at one worker and at four: exit 1 after `out`, the error
/// reported, and the debug runtime's block counts balanced at both.
fn exits_balanced(name: &str, src: &str, out: &str) {
    if !cfg!(unix) {
        // No executor in Velt on Windows on main yet (#2723).
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = exe_name(dir.path(), name);
    build_std(&file, &exe);
    for threads in ["1", "4"] {
        let (code, got, err) = run(&exe, dir.path(), threads);
        assert_eq!(code, 1, "{name}, VELT_THREADS={threads}: {err}");
        assert_eq!(got, out, "{name}, VELT_THREADS={threads}");
        assert!(err.contains("Uncaught Error: boom"), "{name}: {err}");
        let (a, f) = blocks(&err).expect("an rc stats line");
        assert_eq!(
            a, f,
            "{name}, VELT_THREADS={threads}: blocks not balanced: {err}"
        );
    }
}

#[test]
fn an_uncaught_exit_reporting_a_held_promise_balances_the_block_counts() {
    exits_balanced("held_exit", HELD_EXIT, "main ends, dropping p\n");
}

#[test]
fn an_uncaught_exit_while_main_sleeps_balances_the_block_counts() {
    exits_balanced("suspended_exit", SUSPENDED_EXIT, "dropping p\n");
}
