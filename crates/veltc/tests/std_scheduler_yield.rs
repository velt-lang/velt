//! `yieldNow()` across the executor in Velt's modes (std/lang/hooks/sched_yield.velt, #3241): a
//! task's root that yielded in a poll on a lazy worker 0 (`VELT_THREADS=1 VELT_RESCUE=1`) resumes
//! when its next poll is the first one in shared mode, on a spare the stall rescue started.

use sched_workers_support::{build_and_run_with, has_threads, manifest};

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod test_dir;

/// Sixteen tasks whose roots yield at once, polled first by a lazy worker 0 (its polls don't mark
/// a task started, sched_run.velt `run`), then `main` computes until one of them is done: the
/// monitor finds worker 0 stalled, shares its queue and starts a spare, whose polls are the
/// tasks' first in shared mode. Such a poll marks the task started; it once wrote back the flags
/// it read before the poll's begin cleared the yield's `ROOT_YIELDED`, so every task stayed
/// suspended with its wake spent and the program printed `done 0` after its deadline (#3256).
const ROOT_YIELDED_ACROSS_MODES: &str = r#"class Count {
  done: Atomic<i64> = new Atomic<i64>(0);
}

async function yielder(c: shared<Count>): Promise<void> {
  await yieldNow();
  c.done.add(1);
}

async function main() {
  const c = shared(new Count());
  const hs: Promise<void>[] = [];
  for (let i = 0; i < 16; i++) {
    hs.push(spawn(yielder(c)));
  }
  // Their first polls: each yields.
  await yieldNow();
  const t0 = Date.now();
  while (c.done.load() < 16 && Date.now() - t0 < 10000) {}
  console.log("done", c.done.load());
  if (c.done.load() === 16) {
    await Promise.all(hs);
  }
}
"#;

#[test]
fn a_root_that_yielded_on_a_lazy_worker_resumes_in_its_first_shared_poll() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run_with(
        dir.path(),
        "yield_modes",
        ROOT_YIELDED_ACROSS_MODES,
        "1",
        &[("VELT_RESCUE", "1")],
    );
    assert_eq!((code, out.as_str()), (0, "done 16\n"));
}
