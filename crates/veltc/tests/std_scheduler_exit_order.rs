//! The executor in Velt's workers (std/lang/hooks/sched_exit.velt): an uncaught error reported
//! while `main` runs a poll on worker 0 ends the program only after that poll, so what `main`
//! printed comes first and every block is given back, as in Node (#3027). Built with the executor
//! in Velt and run with `VELT_THREADS=4`, like std_scheduler_workers.rs. Each program drives the
//! order it tests with atomics and the run queue, not with time.

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
  // The report comes meanwhile, on worker 1.
  busy(20000000);
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
  busy(2000000);
  console.log("main after busy");
}
"#;
