//! The slot for the next task and the local loop of the executor in Velt's shared mode
//! (std/lang/hooks/sched_queue.velt `pushNext`, `pop`, `popNextOwn`; sched_loop.velt `runLocal`,
//! sched_run.velt `again`, #2786), end to end: two tasks waking each other through the slot let
//! the other tasks of their worker run, and a task woken during its own poll waits behind what
//! its worker has queued. Built with `VELT_SCHEDULER=std`, run with `VELT_THREADS=4`; a program
//! that hangs is killed after a minute.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Workers only start where `#sched-thread` has threads.
fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux", windows))
}

/// `src` built with `VELT_SCHEDULER=std` and run with 4 workers: its exit code and stdout.
fn run_std(name: &str, src: &str) -> (Option<i32>, String) {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir
        .path()
        .join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    let mut child = command(&exe)
        .env("VELT_THREADS", "4")
        .env_remove("VELT_SCHEDULER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A watchdog, not a synchronization: each program ends in milliseconds unless it starves.
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            child.kill().unwrap();
            panic!("{name}: still running after a minute");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    (
        o.status.code(),
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
    )
}

/// A task spawned on a worker where two tasks wake each other through the slot runs within a few
/// rounds: after `NEXT_RUNS` tasks from the slot the queues get their turn, the second queue too.
/// The marker is spawned beside the ping-pong and sets the flag that ends it; before the fix the
/// slot's task went through the input queue and was taken back at once, so the marker ran only
/// once another worker took it, or never (the program hung at 4 workers).
#[test]
fn a_task_spawned_beside_two_tasks_waking_each_other_through_the_slot_runs() {
    if !has_threads() {
        return;
    }
    let (code, out) = run_std("slot_spawn", SLOT_SPAWN);
    assert_eq!(code, Some(0), "{out}");
    let rounds: u64 = out
        .strip_prefix("marker ran after ")
        .and_then(|r| r.strip_suffix(" rounds\n"))
        .and_then(|r| r.parse().ok())
        .unwrap_or_else(|| panic!("{out}"));
    // A turn every `NEXT_RUNS` tasks from the slot: the marker runs within the first rounds.
    // Before the fix it waited until another worker took it (tens of thousands of rounds) or
    // for good.
    assert!(rounds < 1000, "{out}");
}

/// The program of [`a_task_spawned_beside_two_tasks_waking_each_other_through_the_slot_runs`].
const SLOT_SPAWN: &str = r#"import { currentWorker } from "velt/task";
import { channel, Channel } from "velt/sync";
class Flag {
  done: Atomic<i64> = new Atomic<i64>(0);
}
async function marker(f: shared<Flag>): Promise<void> {
  f.done.store(1);
}
async function echo(inbox: Channel<i64>, out: Channel<i64>): Promise<void> {
  while (true) {
    const v = await inbox.receive();
    if (v == null) {
      return;
    }
    out.trySend(v);
  }
}
async function pinger(): Promise<void> {
  const f = shared(new Flag());
  const a = channel<i64>();
  const b = channel<i64>();
  const e = spawn(echo(a, b));
  const s = spawn(marker(f));
  let i: i64 = 0;
  while (f.done.load() == 0) {
    a.trySend(i);
    await b.receive();
    i++;
  }
  a.close();
  await e;
  await s;
  console.log("marker ran after " + i + " rounds");
}
async function main() {
  const t = spawn(pinger());
  const t0 = Date.now();
  while (Date.now() - t0 < 30) {}
  await t;
}
await main();
"#;

/// A sleeper on a worker where two tasks wake each other through the slot wakes and runs: the
/// slot's limit (`NEXT_RUNS`) holds on the path without the lock too. Without it (the slot's task
/// taken whatever its count) the program hung.
#[test]
fn a_sleeper_beside_two_tasks_waking_each_other_through_the_slot_runs() {
    if !has_threads() {
        return;
    }
    let (code, out) = run_std("slot_sleep", SLOT_SLEEP);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.starts_with("sleep ended w"), "{out}");
}

/// The program of [`a_sleeper_beside_two_tasks_waking_each_other_through_the_slot_runs`].
const SLOT_SLEEP: &str = r#"import { currentWorker } from "velt/task";
import { channel, Channel } from "velt/sync";
class Flag {
  done: Atomic<i64> = new Atomic<i64>(0);
}
async function sleeper(f: shared<Flag>): Promise<void> {
  await sleep(5);
  f.done.store(1);
}
async function echo(inbox: Channel<i64>, out: Channel<i64>): Promise<void> {
  while (true) {
    const v = await inbox.receive();
    if (v == null) {
      return;
    }
    out.trySend(v);
  }
}
async function pinger(): Promise<void> {
  const f = shared(new Flag());
  const a = channel<i64>();
  const b = channel<i64>();
  const e = spawn(echo(a, b));
  const s = spawn(sleeper(f));
  await sleep(1);
  let i: i64 = 0;
  while (f.done.load() == 0) {
    a.trySend(i);
    await b.receive();
    i++;
  }
  a.close();
  await e;
  await s;
  console.log("sleep ended w" + currentWorker());
}
async function main() {
  const t = spawn(pinger());
  const t0 = Date.now();
  while (Date.now() - t0 < 30) {}
  await t;
}
await main();
"#;

/// A task woken during its own poll (a `yieldNow`) waits behind what its worker queued
/// meanwhile, also once it runs in the local loop: the task it spawned on its worker runs after
/// two of its polls, as on `main`. It stays in `E_AGAIN` only with its worker's queues empty
/// (sched_run.velt `again`); without that check the local loop polled it again until the next
/// I/O tick (60 polls).
#[test]
fn a_task_that_yields_lets_the_task_it_spawned_on_its_worker_run() {
    if !has_threads() {
        return;
    }
    let (code, out) = run_std("yield_behind", YIELD_BEHIND);
    assert_eq!(code, Some(0), "{out}");
    let polls: u64 = out
        .strip_prefix("polls before the spawned task ran: ")
        .and_then(|r| r.strip_suffix('\n'))
        .and_then(|r| r.parse().ok())
        .unwrap_or_else(|| panic!("{out}"));
    assert!(polls < 10, "{out}");
}

/// The program of [`a_task_that_yields_lets_the_task_it_spawned_on_its_worker_run`].
const YIELD_BEHIND: &str = r#"class Flag {
  ran: Atomic<i64> = new Atomic<i64>(0);
}

async function spawned(f: shared<Flag>): Promise<void> {
  f.ran.store(1);
}

async function yielder(): Promise<void> {
  for (let i = 0; i < 5; i++) {
    await yieldNow();
  }
  const f = shared(new Flag());
  let polls = 0;
  const t = spawn(later(f), { worker: 1 });
  while (f.ran.load() == 0 && polls < 1000) {
    await yieldNow();
    polls++;
  }
  console.log(`polls before the spawned task ran: ${polls}`);
  await t;
}

async function later(f: shared<Flag>): Promise<void> {
  await yieldNow();
  await spawned(f);
}

async function main() {
  await spawn(yielder(), { worker: 1 });
}
"#;
