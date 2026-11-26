//! The executor in Velt's workers (std/lang/hooks/sched_exit.velt): an uncaught error reported
//! while `main` runs a poll on worker 0 ends the program only after that poll, so what `main`
//! printed comes first and every block is given back, as in Node (#3027). Built with the executor
//! in Velt and run with `VELT_THREADS=4`, like std_scheduler_workers.rs. Each program drives the
//! order it tests with atomics and the run queue, not with time.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use sched_workers_support::{build_and_run_with, has_threads, manifest};

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod test_dir;

/// Runs `src` three times at 4 workers (twice with the block counters on): each run exits with 1,
/// prints `main`'s two lines, then the report, nothing after, and the counted runs balance.
fn reports_after_mains_lines(name: &str, src: &str) {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let counted = [("VELT_RT_DEBUG_ALLOC", "1"), ("VELT_RC_STATS", "1")];
    for run in 0..3 {
        let env: &[(&str, &str)] = if run == 0 { &[] } else { &counted };
        let (code, out) = build_and_run_with(dir.path(), name, src, "4", env);
        let blocks = out
            .split_whitespace()
            .find_map(|w| w.strip_prefix("blocks="))
            .and_then(|b| b.split_once('/'));
        assert!(
            code == 1
                && out.starts_with("main line\nmain after busy\n")
                && !out.contains("unreachable")
                && out.contains("Uncaught Failed")
                && (run == 0 || blocks.is_some_and(|(a, f)| a == f)),
            "{name} run {run}: exit {code}\n{out}"
        );
    }
}

/// A task on worker 1 rejects after `main` dropped its handle, while `main` runs a poll on worker
/// 0: the error is reported there and handed to worker 0, which ends the program once that poll
/// ends, as Node orders it (the rejection can only be reported after `main`'s synchronous code).
/// The exit used to end the process from worker 1 at once, losing `main`'s lines, with the root
/// of `main` still counted (#3027).
#[test]
fn an_uncaught_error_on_another_worker_waits_for_mains_poll() {
    reports_after_mains_lines("late", LATE);
}

/// The task rejects before `main` drops its handle: the drop, in `main`'s poll, keeps the result
/// until that poll's microtasks are drained, so the report comes after `main`'s lines. The drop
/// glue used to report and end the program inside the poll (#3027).
#[test]
fn a_rejected_result_dropped_in_mains_poll_reports_after_it() {
    reports_after_mains_lines("early", EARLY);
}

/// The program of [`an_uncaught_error_on_another_worker_waits_for_mains_poll`]: the task waits
/// for the handle's drop (`GO`), `main` for the throw (`THROWN`), then stays in its poll.
const LATE: &str = r#"class Failed extends Error {}

const GO = new Atomic<i64>(0);
const THROWN = new Atomic<i64>(0);
const SINK = new Atomic<i64>(0);

function busy(n: i64): i64 {
  let s: i64 = 0;
  for (let j: i64 = 0; j < n; j++) {
    s = (s * 31 + j) % 1000003;
  }
  return s;
}

async function failing(): Promise<i64> {
  await yieldNow();
  while (GO.load() === 0) {}
  THROWN.store(1);
  throw new Failed("lost");
}

async function main() {
  {
    const task = spawn(failing(), { worker: 1 });
  }
  GO.store(1);
  console.log("main line");
  while (THROWN.load() === 0) {}
  // The report comes meanwhile, on worker 1 (the sum is stored, so the loop stays).
  SINK.store(busy(20000000));
  console.log("main after busy");
  // A hang guard: the rejection ends the process at the end of this poll.
  await sleep(60000);
  console.log("unreachable");
}
"#;

/// The program of [`a_rejected_result_dropped_in_mains_poll_reports_after_it`]: the task, pinned
/// to worker 0 and queued ahead of `main` by its `yieldNow`, rejects in its first poll; `main`
/// then drops its handle and returns in the same poll.
const EARLY: &str = r#"class Failed extends Error {}

const SINK = new Atomic<i64>(0);

function busy(n: i64): i64 {
  let s: i64 = 0;
  for (let j: i64 = 0; j < n; j++) {
    s = (s * 31 + j) % 1000003;
  }
  return s;
}

async function failing(): Promise<i64> {
  throw new Failed("lost");
}

async function main() {
  {
    const task = spawn(failing(), { worker: 0 });
    await yieldNow();
  }
  console.log("main line");
  SINK.store(busy(2000000));
  console.log("main after busy");
}
"#;

/// An uncaught error reported on another worker while worker 0 runs a poll that never ends: the
/// exit handed to worker 0 is taken by the reporting worker once worker 0 has not taken it within
/// its wait (std/lang/hooks/sched_exit.velt `waitForMain`), so the program ends with the report and
/// exit code 1 (#3888: it printed the report and never ended). Each shape of the issue, run twice
/// at 4 workers.
#[test]
fn an_uncaught_error_ends_the_program_while_worker_0_never_yields() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for (name, src) in [
        ("never_yields_sync", NEVER_YIELDS_SYNC),
        ("never_yields_micro", NEVER_YIELDS_MICRO),
    ] {
        for run in 0..2 {
            let (code, out) = build_and_run_bounded(dir.path(), name, src);
            assert!(
                code == Some(1)
                    && out.contains("Uncaught Error: boom")
                    && !out.contains("unreachable"),
                "{name} run {run}: exit {code:?} (None: still running after a minute)\n{out}"
            );
        }
    }
}

/// `src` built with `VELT_SCHEDULER=std` and run at 4 workers, killed if it still runs after a
/// minute: its exit code (`None`: killed) and output.
fn build_and_run_bounded(dir: &Path, name: &str, src: &str) -> (Option<i32>, String) {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!("{name}_std{}", std::env::consts::EXE_SUFFIX));
    let o = no_window::command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let mut child = no_window::command(&exe)
        .env("VELT_THREADS", "4")
        .env_remove("VELT_SCHEDULER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A watchdog, not a synchronization: the program that never ends would hang the test run.
    let start = Instant::now();
    let mut killed = false;
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            child.kill().unwrap();
            killed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    let err = String::from_utf8_lossy(&o.stderr);
    let code = if killed { None } else { o.status.code() };
    (code, format!("{out}{err}"))
}

/// A program of [`an_uncaught_error_ends_the_program_while_worker_0_never_yields`]: `main`, on
/// worker 0, loops for ever without an await; a task on worker 1 throws, its result unclaimed.
const NEVER_YIELDS_SYNC: &str = r#"const FOREVER = new Atomic<i64>(0);
async function thrower(): Promise<void> {
  await yieldNow();
  throw new Error("boom");
}
async function main() {
  spawn(thrower(), { worker: 1 });
  while (FOREVER.load() === 0) {}
  console.log("unreachable");
}
"#;

/// A program of [`an_uncaught_error_ends_the_program_while_worker_0_never_yields`]: `main` and two
/// tasks on workers 1 and 2 loop on `await Promise.resolve()` (a poll whose microtasks never end);
/// a task on worker 3 throws, its result unclaimed.
const NEVER_YIELDS_MICRO: &str = r#"async function spin(): Promise<void> {
  while (true) {
    await Promise.resolve();
  }
}
async function thrower(): Promise<void> {
  await yieldNow();
  throw new Error("boom");
}
async function main() {
  const a = spawn(spin(), { worker: 1 });
  const b = spawn(spin(), { worker: 2 });
  spawn(thrower(), { worker: 3 });
  await spin();
  await a;
  await b;
  console.log("unreachable");
}
"#;
