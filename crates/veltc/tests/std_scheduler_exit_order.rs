//! The executor in Velt's workers (std/lang/hooks/sched_exit.velt): an uncaught error reported on
//! another worker ends the program only after main's current poll, so what main printed comes
//! first and every block is given back, as in Node (#3027). Built with the executor in Velt and
//! run with `VELT_THREADS=4`, like std_scheduler_workers.rs.

use sched_workers_support::{build_and_run_with, has_threads, manifest};

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod test_dir;

/// An uncaught error reported on another worker while `main` runs a poll on worker 0 ends the
/// program once that poll ends, as Node orders it (the rejection can only be reported after
/// `main`'s synchronous code): both of `main`'s lines are printed, and with the counters on,
/// every block is given back. The exit used to end the process from the reporting worker at once,
/// losing what `main` had printed but not published yet, with the root of `main` still counted
/// (#3027). Before the fix every run lost the lines.
#[test]
fn an_uncaught_error_on_another_worker_waits_for_mains_poll() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let counted = [("VELT_RT_DEBUG_ALLOC", "1"), ("VELT_RC_STATS", "1")];
    for run in 0..3 {
        let env: &[(&str, &str)] = if run == 0 { &[] } else { &counted };
        let (code, out) = build_and_run_with(dir.path(), "mid_poll", MID_POLL, "4", env);
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
            "run {run}: exit {code}\n{out}"
        );
    }
}

/// The program of [`an_uncaught_error_on_another_worker_waits_for_mains_poll`].
const MID_POLL: &str = r#"class Failed extends Error {}

function busy(n: i64): i64 {
  let s: i64 = 0;
  for (let j: i64 = 0; j < n; j++) {
    s = (s * 31 + j) % 1000003;
  }
  return s;
}

async function failing(): Promise<i64> {
  await yieldNow();
  throw new Failed("lost");
}

async function main() {
  {
    const task = spawn(failing(), { worker: 1 });
  }
  console.log("main line");
  // Long enough for the task to reject on worker 1 meanwhile.
  busy(20000000);
  console.log("main after busy");
  // A hang guard: the rejection ends the process at the end of this poll.
  await sleep(60000);
  console.log("unreachable");
}
"#;
