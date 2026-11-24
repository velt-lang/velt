//! A wake by value from a thread that is not the task's home worker hands the waker's reference
//! to the task's next poll, which gives it back before any park counts (#3487,
//! std/lang/hooks/sched_wake_away.velt). Before, the woken task could run and park on its own
//! worker between the wake and the release, its park counted the waker's reference, and the
//! release that followed gave it back with no poll: a task parked for ever stayed to the end of
//! the program. That window opens only when the waking thread loses its core right after the wake
//! (a busy machine); `VELT_SCHED_WAKE_PAUSE`, read with the block counters on, holds it open after
//! every wake from another thread, so each run goes through it. Each program runs counted with the
//! checking allocator at 4 and 8 workers: exit 0, its output, and every block freed.

use std::path::{Path, PathBuf};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Workers only start where `#sched-thread` has threads (macOS, Linux and Windows).
fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux", windows))
}

/// `src` built with `VELT_SCHEDULER=std` in `dir`: the executable.
fn build_std(dir: &Path, name: &str, src: &str) -> PathBuf {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!(
        "{name}_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
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

/// Every block allocated was freed (`VELT_RC_STATS`'s `blocks=allocated/freed`).
fn all_freed(err: &str) -> bool {
    err.split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))
        .and_then(|b| b.split_once('/'))
        .is_some_and(|(a, f)| a == f)
}

/// Builds `src` and runs it `runs` times at 4 and at 8 workers, counted, with the window after a
/// wake from another thread held open (2,000 turns of the core): exit 0, `want` printed, every
/// block freed.
fn check(name: &str, src: &str, want: &str, runs: usize) {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), name, src);
    let mut bad = Vec::new();
    for threads in ["4", "8"] {
        for i in 0..runs {
            let o = command(&exe)
                .env("VELT_THREADS", threads)
                .env("VELT_RT_DEBUG_ALLOC", "1")
                .env("VELT_RC_STATS", "1")
                .env("VELT_SCHED_WAKE_PAUSE", "2000")
                .env_remove("VELT_RESCUE")
                .env_remove("VELT_SCHEDULER")
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
            let err = String::from_utf8_lossy(&o.stderr);
            if o.status.code() != Some(0) || out != want || !all_freed(&err) {
                bad.push(format!(
                    "{threads} workers, run {i}: exit {:?}, stdout {out:?}, stderr {err}",
                    o.status.code()
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{name}: {} runs went wrong:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// A task's root waker woken by value on worker 2 (the #3485 review's `awaited_dropped`): the
/// task then parks for ever on a holder kept in module state. With the old order every run left
/// blocks (on a busy Linux machine 36 to 54 runs of 100 did, without the pause).
#[test]
fn a_task_woken_from_another_worker_is_found_parked_for_ever() {
    check("awaited_dropped", AWAITED_DROPPED, "dropped 30\n", 3);
}

/// A promise's node waker woken by value on worker 3, the promise not adopted by its owner: it
/// then waits for ever. With the old order every run left blocks.
#[test]
fn a_promise_woken_from_another_worker_is_found_owner_only() {
    check("node_woken_away", NODE_WOKEN_AWAY, "done 20\n", 3);
}

/// Tasks woken from worker 3 while their home worker runs synchronous code, so another worker may
/// take them from its queue: the reference each wake hands the task's next poll goes back once,
/// wherever that poll runs (sched_wake_away.velt `polled`). Giving it back twice, or not at all,
/// left blocks in most runs.
#[test]
fn a_woken_task_taken_by_another_worker_gives_the_reference_back_once() {
    check("stolen_carry", STOLEN_CARRY, "parked 40 true\n", 5);
}

/// `main` ends while a task woken from another worker is still queued behind a busy one on its
/// home worker: the program still runs it, and its poll gives the carried reference back.
#[test]
#[cfg_attr(target_os = "macos", ignore = "flaky on loaded macOS runners: #3690")]
fn a_carried_wake_still_queued_when_main_ends_is_given_back() {
    check("exit_queued", EXIT_QUEUED, "main ends\nbusy true\n", 3);
}

/// The program of [`a_task_woken_from_another_worker_is_found_parked_for_ever`].
const AWAITED_DROPPED: &str = r#"// A holder handed to worker 2 is awaited once there (a promise that settles), then the task
// keeps a second holder of a dead promise in module state, which main drops later, off its task.
class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

const KEPT: Holder[] = [];
const PARKED = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function soon(): Promise<number> {
  await yieldNow();
  return 1;
}

async function user(h: Holder): Promise<number> {
  return await h.p;
}

async function maker(): Promise<number> {
  const live = new Holder(soon());
  const got = await spawn(user(live), { worker: 2 });
  const d = new Holder(dead());
  KEPT.push(d);
  PARKED.add(1);
  return got + (await d.p);
}

async function main() {
  const n: i64 = 30;
  for (let r: i64 = 0; r < n; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  while (PARKED.load() < n) {
    await yieldNow();
  }
  for (let i = 0; i < 50; i++) {
    await yieldNow();
  }
  let dropped: i64 = 0;
  while (KEPT.length > 0) {
    KEPT.pop();
    dropped += 1;
  }
  console.log(`dropped ${dropped}`);
}
"#;

/// The program of [`a_promise_woken_from_another_worker_is_found_owner_only`].
const NODE_WOKEN_AWAY: &str = r#"// #3487 probe: a promise of the task (`p`, not adopted: its owner yields first) waits on a promise
// whose resolve function it hands to a task on worker 3 (no module state, so no module lock), which
// calls it: `p`'s node waker is woken by value from worker 3. Then `p` waits for ever. Its owner
// awaits it after. Every task should be found parked for ever.

const DONE = new Atomic<i64>(0);

async function settle(r: (v: number) => void): Promise<void> {
  await yieldNow();
  await yieldNow();
  r(1);
}

async function waitOnce(): Promise<number> {
  const a = await new Promise<number>((resolve) => {
    void spawn(settle(resolve), { worker: 3 });
  });
  DONE.add(1);
  return a + (await new Promise<number>((resolve) => {}));
}

async function maker(): Promise<number> {
  const p = waitOnce();
  await yieldNow();
  return await p;
}

async function main() {
  const n: i64 = 20;
  for (let r: i64 = 0; r < n; r++) {
    void spawn(maker(), { worker: 1 + (r % 2) });
  }
  while (DONE.load() < n) {
    await yieldNow();
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log(`done ${DONE.load()}`);
}
"#;

/// The program of [`a_woken_task_taken_by_another_worker_gives_the_reference_back_once`].
const STOLEN_CARRY: &str = r#"// #3487 (b): tasks woken by value from worker 3 while their home worker (0) runs synchronous code,
// so other workers may take them from its queue: each wake hands its reference to the woken
// task's next poll, wherever that runs, and it goes back once. Then each waits for ever and is
// found parked for ever.

const READY = new Atomic<i64>(0);
const PARKED = new Atomic<i64>(0);

async function dead(): Promise<number> {
  return await new Promise<number>((resolve) => {});
}

async function waiter(p: Promise<number>): Promise<number> {
  READY.add(1);
  const v = await p;
  PARKED.add(1);
  return v + (await dead());
}

async function settle(rs: ((v: number) => void)[], n: i64): Promise<void> {
  while (READY.load() < n) {
    await yieldNow();
  }
  for (const r of rs) {
    r(1);
  }
}

async function main() {
  const n: i64 = 40;
  const rs: ((v: number) => void)[] = [];
  for (let i: i64 = 0; i < n; i++) {
    const { promise, resolve } = Promise.withResolvers<number>();
    rs.push(resolve);
    void spawn(waiter(promise));
  }
  while (READY.load() < n) {
    await yieldNow();
  }
  void spawn(settle(rs, n), { worker: 3 });
  rs.length = 0;
  // Worker 0 busy while the wakes come.
  let x: i64 = 0;
  for (let i: i64 = 0; i < 20000000; i++) {
    x += i;
  }
  while (PARKED.load() < n) {
    await yieldNow();
  }
  for (let y = 0; y < 200; y++) {
    await yieldNow();
  }
  console.log(`parked ${PARKED.load()} ${x > 0}`);
}
"#;

/// The program of [`a_carried_wake_still_queued_when_main_ends_is_given_back`].
const EXIT_QUEUED: &str = r#"// #3487 (b): `main` ends while a task woken by value from another worker is still queued: its
// home worker (1) is busy in a long synchronous loop, so the wake (from worker 3, carrying the
// waker's reference) leaves it queued, and `main` returns before worker 1 polls it.

const WOKEN = new Atomic<i64>(0);
const READY = new Atomic<i64>(0);

async function waiter(p: Promise<number>): Promise<number> {
  READY.add(1);
  return await p;
}

async function busy(): Promise<void> {
  while (WOKEN.load() === 0) {
    // Spins until the wake below happened, then a while longer.
  }
  let x: i64 = 0;
  for (let i: i64 = 0; i < 50000000; i++) {
    x += i;
  }
  console.log(`busy ${x > 0}`);
}

async function settle(r: (v: number) => void): Promise<void> {
  while (READY.load() === 0) {
    await yieldNow();
  }
  r(1);
  WOKEN.add(1);
}

async function main() {
  const { promise, resolve } = Promise.withResolvers<number>();
  void spawn(waiter(promise), { worker: 1 });
  void spawn(settle(resolve), { worker: 3 });
  void spawn(busy(), { worker: 1 });
  while (WOKEN.load() === 0) {
    await yieldNow();
  }
  console.log("main ends");
}
"#;
