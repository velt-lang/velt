//! A sleep handed to another task (#3207, std/lang/hooks/sched_sleep_move.velt): a promise that
//! holds a sleep is polled by whichever task awaits it now, and dropped by whoever holds it
//! last, on that task's worker, which need not be the one whose timer block holds the sleep.
//! Such a poll or drop goes through the holding worker. The runs are counted
//! (`VELT_RC_STATS=1`), which turns on std's check of the rule (a sleep is touched only on the
//! worker that holds it, and expires for a task of that worker: an `ICE:` otherwise), so a
//! program that breaks it fails every time, not only when memory happens to break.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use no_window::command;
use sched_workers_support::{has_threads, manifest};

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod test_dir;

/// `src` built under the executor in Velt in `dir`: the executable.
fn build(dir: &Path, name: &str, src: &str) -> PathBuf {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    let o = command(env!("CARGO_BIN_EXE_velt"))
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
    exe
}

/// Runs `exe` with `threads` workers, counted and with the checking allocator, killed after a
/// minute: its exit code (`None` when killed) and output.
fn run(exe: &Path, threads: &str) -> (Option<i32>, String) {
    let mut child = command(exe)
        .env_remove("VELT_SCHEDULER")
        .env_remove("VELT_RESCUE")
        .env("VELT_THREADS", threads)
        .env("VELT_RC_STATS", "1")
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        // timing: polls the child for its exit under a one-minute cap, assumes no timing
        std::thread::sleep(Duration::from_millis(20));
    };
    let o = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (code, text)
}

/// Whether the `rc stats:` line shows as many blocks freed as allocated.
fn balanced(out: &str) -> bool {
    out.lines()
        .find(|l| l.starts_with("rc stats:"))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("blocks=")))
        .and_then(|b| b.split_once('/'))
        .is_some_and(|(a, f)| a == f)
}

/// Runs `exe` `runs` times with `threads` workers: exit 0, output starting with `want`, blocks
/// balanced.
fn check(exe: &Path, threads: &str, runs: usize, want: &str) {
    for i in 0..runs {
        let (code, out) = run(exe, threads);
        assert_eq!(code, Some(0), "run {i} at {threads} workers: {out}");
        assert!(out.starts_with(want), "run {i} at {threads} workers: {out}");
        assert!(
            balanced(&out),
            "run {i} at {threads} workers, blocks: {out}"
        );
    }
}

/// A task pinned to worker 1 polls a sleep (pending in worker 1's block from then on) and hands
/// its promise to a task pinned to worker 2, which awaits it: worker 1 gives the sleep up, and it
/// fires in worker 2's block. Without the hand-over the sleep expired on worker 1 for a task of
/// worker 2 (the check's `ICE:`, every run).
#[test]
fn a_sleep_awaited_on_another_worker_moves_to_it() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "pinned_poll", PINNED_POLL);
    check(&exe, "4", 10, "awaited on worker 2\n");
}

/// As above, but the task on worker 2 drops the promise unawaited: worker 1 takes the sleep off
/// its block and frees it, and the program ends at once instead of waiting ten minutes for it
/// (the run's limit is one). Without the hand-over worker 2 unlinked it from a list it was not
/// in (the check's `ICE:`, every run).
#[test]
fn a_sleep_dropped_on_another_worker_leaves_its_block() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "pinned_drop", PINNED_DROP);
    check(&exe, "4", 10, "dropped on worker 2\n");
}

/// A sleep pending in worker 1's block awaited by three tasks at once, two on worker 2 and one on
/// worker 0: each asks worker 1, while another's request may be on its way or just served (the
/// sleep then new again, in the asker's block, or still in worker 1's). Every task wakes once.
/// Without the hand-over the sleep expired on worker 1 for a task of another worker (the check's
/// `ICE:`, every run).
#[test]
fn a_sleep_awaited_from_three_tasks_on_other_workers_wakes_them_all() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "pinned_three", PINNED_THREE);
    check(&exe, "4", 10, "awaited on workers 2 2 0\n");
}

/// The shapes that crashed (#3201's review): tasks hand polled sleeps on while busy tasks make
/// worker 0 offer its queue, so the second poller is fresh on another worker, runs in its waker's
/// slot, or is offered between its polls; and two tasks await one sleep, each asking for it while
/// the other's request may be under way. Every sleep fires once, at 4 and 8 workers.
#[test]
fn handed_on_sleeps_fire_under_offers() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for (name, src) in [
        ("handed_user", HANDED_USER),
        ("handed_maker", HANDED_MAKER),
        ("handed_two", HANDED_TWO),
    ] {
        let exe = build(dir.path(), name, src);
        check(&exe, "4", 4, "fired 1000\n");
        check(&exe, "8", 4, "fired 1000\n");
    }
}

/// The program of [`a_sleep_awaited_on_another_worker_moves_to_it`].
const PINNED_POLL: &str = r#"import { currentWorker } from "velt/task";

class Holder {
  p: Promise<void>;
  constructor(p: Promise<void>) {
    this.p = p;
  }
}

// On worker 1: polls the sleep there (it is pending in worker 1's timer block), then hands it on.
async function first(): Promise<Holder> {
  const s = sleep(20);
  await Promise.race([s, yieldNow()]);
  return new Holder(s);
}

// On worker 2: awaits the sleep worker 1's block holds.
async function second(h: Holder): Promise<i64> {
  await h.p;
  return currentWorker();
}

async function main() {
  const h = await spawn(first(), { worker: 1 });
  const w = await spawn(second(h), { worker: 2 });
  console.log(`awaited on worker ${w}`);
}
"#;

/// The program of [`a_sleep_dropped_on_another_worker_leaves_its_block`].
const PINNED_DROP: &str = r#"import { currentWorker } from "velt/task";

class Holder {
  p: Promise<void>;
  constructor(p: Promise<void>) {
    this.p = p;
  }
}

// On worker 1: polls the sleep there (it is pending in worker 1's timer block), then hands it on.
async function first(): Promise<Holder> {
  const s = sleep(600000);
  await Promise.race([s, yieldNow()]);
  return new Holder(s);
}

// On worker 2: drops the sleep worker 1's block holds, unawaited (the holder ends with the task).
async function second(h: Holder): Promise<i64> {
  await yieldNow();
  return currentWorker();
}

async function main() {
  const h = await spawn(first(), { worker: 1 });
  const w = await spawn(second(h), { worker: 2 });
  // The program ends now, not in ten minutes: the dropped sleep left worker 1's block.
  console.log(`dropped on worker ${w}`);
}
"#;

/// The program of [`a_sleep_awaited_from_three_tasks_on_other_workers_wakes_them_all`].
const PINNED_THREE: &str = r#"import { currentWorker } from "velt/task";

class Holder {
  p: Promise<void>;
  constructor(p: Promise<void>) {
    this.p = p;
  }
}

class Trio {
  a: Holder;
  b: Holder;
  c: Holder;
  constructor(p: Promise<void>) {
    this.a = new Holder(p);
    this.b = new Holder(p);
    this.c = new Holder(p);
  }
}

// On worker 1: polls the sleep there (it is pending in worker 1's timer block), then hands it on.
async function first(): Promise<Trio> {
  const s = sleep(20);
  await Promise.race([s, yieldNow()]);
  return new Trio(s);
}

// Three tasks, none on worker 1, await the same sleep: each asks the worker that holds it, while
// another's request for it may be on its way or just served.
async function waits(h: Holder): Promise<i64> {
  await h.p;
  return currentWorker();
}

async function main() {
  const trio = await spawn(first(), { worker: 1 });
  const a = spawn(waits(trio.a), { worker: 2 });
  const b = spawn(waits(trio.b), { worker: 2 });
  const c = spawn(waits(trio.c), { worker: 0 });
  console.log(`awaited on workers ${await a} ${await b} ${await c}`);
}
"#;

/// [`handed_on_sleeps_fire_under_offers`]: a task that ended polled the sleep first; another awaits it.
const HANDED_USER: &str = r#"class Gate {
  stop: Atomic<i64> = new Atomic<i64>(0);
  fired: Atomic<i64> = new Atomic<i64>(0);
}

class Holder {
  p: Promise<void>;
  constructor(p: Promise<void>) {
    this.p = p;
  }
}

async function busy(gate: shared<Gate>): Promise<void> {
  while (gate.stop.load() == 0) {
    let x = 0;
    for (let i = 0; i < 20000; i++) {
      x += i % 7;
    }
    if (x < 0) {
      console.log("never");
    }
    await yieldNow();
  }
}

async function maker(gate: shared<Gate>, i: i64): Promise<Holder> {
  const s = sleep(5 + (i % 7));

  await Promise.race([s, yieldNow()]);
  return new Holder(s);
}

// Anchored by a pending sleep, queued again and again: what offers carry.
async function anchored(i: i64): Promise<void> {
  const long = sleep(30 + (i % 5));
  const short = sleep(1);
  await Promise.race([long, short]);
  for (let y = 0; y < 40; y++) {
    await yieldNow();
  }
}

async function user(gate: shared<Gate>, i: i64): Promise<void> {
  const h = await spawn(maker(gate, i));
  await Promise.race([h.p, yieldNow()]);
  for (let y = 0; y < 30 + (i % 7); y++) {
    await yieldNow();
  }
  await h.p;
  gate.fired.add(1);
}

async function main() {
  const gate = shared(new Gate());
  const busyOnes: Promise<void>[] = [];
  for (let i = 0; i < 8; i++) {
    busyOnes.push(spawn(busy(gate)));
  }
  for (let round = 0; round < 5; round++) {
    const ps: Promise<void>[] = [];
    for (let i = 0; i < 200; i++) {
      ps.push(spawn(user(gate, i)));
      ps.push(spawn(anchored(i)));
    }
    await Promise.all(ps);
  }
  gate.stop.store(1);
  await Promise.all(busyOnes);
  console.log(`fired ${gate.fired.load()}`);
}
"#;

/// [`handed_on_sleeps_fire_under_offers`]: the task that polled the sleep first hands it on and yields on.
const HANDED_MAKER: &str = r#"class Gate {
  stop: Atomic<i64> = new Atomic<i64>(0);
  fired: Atomic<i64> = new Atomic<i64>(0);
}

class Holder {
  p: Promise<void>;
  constructor(p: Promise<void>) {
    this.p = p;
  }
}

async function busy(gate: shared<Gate>): Promise<void> {
  while (gate.stop.load() == 0) {
    let x = 0;
    for (let i = 0; i < 20000; i++) {
      x += i % 7;
    }
    if (x < 0) {
      console.log("never");
    }
    await yieldNow();
  }
}

async function user(gate: shared<Gate>, h: Holder): Promise<void> {
  await Promise.race([h.p, yieldNow()]);
  await h.p;
  gate.fired.add(1);
}

async function maker(gate: shared<Gate>, i: i64): Promise<void> {
  const s = sleep(8 + (i % 5));
  await Promise.race([s, yieldNow()]);
  const u = spawn(user(gate, new Holder(s)));
  for (let y = 0; y < 40; y++) {
    await yieldNow();
  }
  await u;
}

async function main() {
  const gate = shared(new Gate());
  const busyOnes: Promise<void>[] = [];
  for (let i = 0; i < 8; i++) {
    busyOnes.push(spawn(busy(gate)));
  }
  for (let round = 0; round < 5; round++) {
    const ps: Promise<void>[] = [];
    for (let i = 0; i < 200; i++) {
      ps.push(spawn(maker(gate, i)));
    }
    await Promise.all(ps);
  }
  gate.stop.store(1);
  await Promise.all(busyOnes);
  console.log(`fired ${gate.fired.load()}`);
}
"#;

/// [`handed_on_sleeps_fire_under_offers`]: two tasks await one sleep the task that polled it first handed on.
const HANDED_TWO: &str = r#"class Gate {
  stop: Atomic<i64> = new Atomic<i64>(0);
  fired: Atomic<i64> = new Atomic<i64>(0);
}

class Holder {
  p: Promise<void>;
  constructor(p: Promise<void>) {
    this.p = p;
  }
}

class Pair {
  a: Holder;
  b: Holder;
  constructor(p: Promise<void>) {
    this.a = new Holder(p);
    this.b = new Holder(p);
  }
}

async function busy(gate: shared<Gate>): Promise<void> {
  while (gate.stop.load() == 0) {
    let x = 0;
    for (let i = 0; i < 20000; i++) {
      x += i % 7;
    }
    if (x < 0) {
      console.log("never");
    }
    await yieldNow();
  }
}

// Polls the sleep, then hands its promise to two tasks.
async function maker(i: i64): Promise<Pair> {
  const s = sleep(5 + (i % 7));
  await Promise.race([s, yieldNow()]);
  return new Pair(s);
}

// Two of these await one sleep: each polls it, offered between its polls, while the other's
// request for it may be on its way.
async function user(gate: shared<Gate>, h: Holder, i: i64): Promise<void> {
  await Promise.race([h.p, yieldNow()]);
  for (let y = 0; y < 20 + (i % 5); y++) {
    await yieldNow();
  }
  await h.p;
  gate.fired.add(1);
}

async function both(gate: shared<Gate>, i: i64): Promise<void> {
  const pair = await spawn(maker(i));
  const a = spawn(user(gate, pair.a, i));
  const b = spawn(user(gate, pair.b, i + 3));
  await a;
  await b;
}

async function main() {
  const gate = shared(new Gate());
  const busyOnes: Promise<void>[] = [];
  for (let i = 0; i < 8; i++) {
    busyOnes.push(spawn(busy(gate)));
  }
  for (let round = 0; round < 5; round++) {
    const ps: Promise<void>[] = [];
    for (let i = 0; i < 100; i++) {
      ps.push(spawn(both(gate, i)));
    }
    await Promise.all(ps);
  }
  gate.stop.store(1);
  await Promise.all(busyOnes);
  console.log(`fired ${gate.fired.load()}`);
}
"#;
