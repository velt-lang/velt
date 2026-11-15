//! The executor in Velt's workers (std/lang/hooks/sched_join.velt): a joined task's output and
//! microtasks come before what its joiner prints after the join, as in Node (#3020). Built with
//! the executor in Velt and run with `VELT_THREADS=4`, like std_scheduler_workers.rs.

use sched_workers_support::{build_and_run, contended_runs_checked, has_threads, manifest};

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod test_dir;

/// What a spawned task printed, its microtasks too, comes before what its joiner prints after the
/// join, as in Node (the task's microtasks run before the awaiter's continuation). Each child
/// resumes on an owner's worker, prints `c`, queues a long microtask that prints `q`, and returns;
/// `main` works a little before it awaits the child, so it often finds the join done at its first
/// poll of the handle, with no wake. The join slot used to say done when the root returned,
/// before the microtasks and the publish of the poll's output, so `main` printed `m` before the
/// child's lines (#3020): about 150 of 2000 rounds on Windows at 4 workers, and none since.
#[test]
fn a_joined_tasks_output_and_microtasks_come_before_its_joiners() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(dir.path(), "joined", JOINED_FIRST, "4");
    assert_eq!((code, joined_first(&out)), (0, Ok(())), "joined order");
    let lost = contended_runs_checked(dir.path(), "joined", "4", joined_first);
    assert!(lost.is_empty(), "{}", lost.join("\n"));
}

/// Whether [`JOINED_FIRST`] printed `c <i>`, `q <i>`, `m <i>` for every round, in that order.
fn joined_first(out: &str) -> Result<(), String> {
    let want: String = (0..500).map(|i| format!("c {i}\nq {i}\nm {i}\n")).collect();
    if out == want {
        return Ok(());
    }
    let at = (out.lines().zip(want.lines()))
        .position(|(a, b)| a != b)
        .unwrap_or(0);
    let got: Vec<_> = out.lines().skip(at).take(3).collect();
    Err(format!("line {at}: {got:?}"))
}

/// The program of [`a_joined_tasks_output_and_microtasks_come_before_its_joiners`].
const JOINED_FIRST: &str = r#"import { channel, Channel } from "velt/sync";

function busy(n: i64): i64 {
  let s: i64 = 0;
  for (let j: i64 = 0; j < n; j++) {
    s = (s * 31 + j) % 1000003;
  }
  return s;
}

async function child(i: i64, out: Channel<() => void>, ready: Channel<i64>): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  out.trySend(() => resolve());
  ready.trySend(i);
  await promise;
  console.log(`c ${i}`);
  queueMicrotask(() => {
    busy(100000);
    console.log(`q ${i}`);
  });
}

async function owner(inbox: Channel<() => void>): Promise<void> {
  while (true) {
    const f = await inbox.receive();
    if (f == null) {
      return;
    }
    f();
  }
}

async function main() {
  const inboxes: Channel<() => void>[] = [];
  const owners: Promise<void>[] = [];
  for (let i = 0; i < 3; i++) {
    const ch = channel<() => void>();
    inboxes.push(ch);
    owners.push(spawn(owner(ch)));
  }
  for (let i: i64 = 0; i < 500; i++) {
    const ready = channel<i64>(1);
    const h = spawn(child(i, inboxes[(i % 3) as i32], ready));
    await ready.receive();
    busy(10000);
    await h;
    console.log(`m ${i}`);
  }
  for (const ch of inboxes) {
    ch.close();
  }
  for (const o of owners) {
    await o;
  }
}
"#;
