//! Started promises handed to another task in the executor in Velt (std/lang/hooks/sched_node.velt,
//! #2881, #2891): a promise made by `Promise.withResolvers` starts on the task that made it, and a
//! task it is passed to awaits or drops it, maybe on another worker while the first task finishes
//! it. The finish and the await meet only in the node's flags and awaiter word, which must be
//! claimed atomically, or the awaiting task is never woken. Each program runs several times at 2
//! workers with the checking allocator and the block count (`VELT_RT_DEBUG_ALLOC=1`,
//! `VELT_RC_STATS=1`): the result printed, every block freed.

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

/// Runs `exe` with `threads` workers (the default rescue), the checking allocator and the block
/// count: its exit code, stdout and stderr.
fn run(exe: &Path, threads: &str) -> (i32, String, String) {
    let o = command(exe)
        .env("VELT_THREADS", threads)
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RC_STATS", "1")
        .env_remove("VELT_RESCUE")
        .env_remove("VELT_SCHEDULER")
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Every block allocated was freed (`VELT_RC_STATS`'s `blocks=allocated/freed`).
fn all_freed(err: &str) -> bool {
    err.split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))
        .and_then(|b| b.split_once('/'))
        .is_some_and(|(a, f)| a == f)
}

/// Runs of [`RESOLVES`]: on `main` before the fix, from 1 run in 3 (a loaded arm64 Mac) to 9 in
/// 10 (x86 Linux) lost the wake at 2 workers; 20 runs leave a broken std about a 0.1% chance to
/// pass them all on the slower of those.
const RUNS: usize = 20;

/// A round's poll opens 100k latches, among them the first and second promise of 1000 waiter
/// tasks, and spins until a task spawned behind it ran: at 2 workers the stall rescue hands that
/// task over, and the waiters, woken by the poll's microtasks finishing their first promise, run
/// on the other worker and await their second promise while the poll's task finishes it. Before
/// the fix, a waiter could read the node's flags before the finish set FINISHED and store its
/// waker after the finish found the awaiter word empty: never woken, so `Promise.all` and `main`
/// waited for ever, and the program ended through the end scan with exit code 0, no output and
/// about 14,000 blocks unfreed (#2891).
#[test]
fn a_waiter_awaiting_a_promise_finished_on_another_worker_is_woken() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "resolves", RESOLVES);
    let mut bad = Vec::new();
    for i in 0..RUNS {
        let (code, out, err) = run(&exe, "2");
        if code != 0 || out != "waited 9990000, marked 10\n" || !all_freed(&err) {
            bad.push(format!(
                "run {i}: exit {code}, stdout {out:?}, stderr {err}"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {RUNS} runs went wrong:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// Runs of [`DROPS`]: with `main`'s std before the fix, 15 of 30 runs (a loaded arm64 Mac) left
/// blocks unfreed; 12 runs leave a broken std about a 0.02% chance to pass them all.
const DROP_RUNS: usize = 12;

/// As [`a_waiter_awaiting_a_promise_finished_on_another_worker_is_woken`], but the waiters never
/// await their second promise: each drops it as it returns, while the poll's task finishes it on
/// the other worker (#2881's result drop). A started promise per waiter (`tick`) runs between the
/// finish of its two promises and spins until the waiter, woken by the first, is about to return,
/// so the drop and the finish meet. Before the fix the drop read the flags without FINISHED and
/// the finish read them without DETACHED: neither dropped the result (a block unfreed).
#[test]
fn a_promise_dropped_while_another_worker_finishes_it_drops_its_result_once() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "drops", DROPS);
    let mut bad = Vec::new();
    for i in 0..DROP_RUNS {
        let (code, out, err) = run(&exe, "2");
        if code != 0 || out != "waited 718800, marked 4\n" || !all_freed(&err) {
            bad.push(format!(
                "run {i}: exit {code}, stdout {out:?}, stderr {err}"
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {DROP_RUNS} runs went wrong:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The program of [`a_waiter_awaiting_a_promise_finished_on_another_worker_is_woken`] (#2888's
/// `a_million_resolves_in_one_poll_are_rescued_mid_loop`).
const RESOLVES: &str = r#"const MARKED = new Atomic<i64>(0);

async function marker(): Promise<void> {
  MARKED.add(1);
}

async function waiter(a: Promise<i64>, b: Promise<i64>): Promise<i64> {
  const x = await a;
  return x + (await b);
}

async function round(r: i64): Promise<i64> {
  const waiters: Promise<i64>[] = [];
  const firsts: ((v: i64) => void)[] = [];
  const seconds: ((v: i64) => void)[] = [];
  for (let i = 0; i < 1000; i++) {
    const a = Promise.withResolvers<i64>();
    const b = Promise.withResolvers<i64>();
    firsts.push(a.resolve);
    seconds.push(b.resolve);
    waiters.push(spawn(waiter(a.promise, b.promise)));
  }
  // The waiters wait.
  await yieldNow();
  const m = spawn(marker());
  let k: i64 = 0;
  for (let i = 0; i < 100000; i++) {
    if (i % 50 == 0) {
      // The first promise of waiter j, then (50 openings later) its second.
      const j = k >> 1;
      if ((k & 1) == 0) {
        const f = firsts[j];
        f(j);
      } else {
        const f = seconds[j];
        f(j);
      }
      k++;
    } else {
      const p = Promise.withResolvers<i64>();
      p.resolve(i);
    }
  }
  while (MARKED.load() <= r) {}
  await m;
  let sum: i64 = 0;
  for (const v of await Promise.all(waiters)) {
    sum += v;
  }
  return sum;
}

async function main() {
  let sum: i64 = 0;
  for (let r = 0; r < 10; r++) {
    sum += await round(r);
    // The spares go idle and retire; worker 0 becomes lazy again.
    await sleep(40);
  }
  console.log(`waited ${sum}, marked ${MARKED.load()}`);
}
"#;

/// The program of [`a_promise_dropped_while_another_worker_finishes_it_drops_its_result_once`].
const DROPS: &str = r#"const MARKED = new Atomic<i64>(0);
const READY = new Atomic<i64>(-1);

class Val {
  v: i64;
  constructor(v: i64) {
    this.v = v;
  }
}

async function marker(): Promise<void> {
  MARKED.add(1);
}

// Drops `b` as it returns, while the round's task finishes it.
async function waiter(j: i64, a: Promise<Val>, b: Promise<Val>): Promise<i64> {
  const x = await a;
  READY.store(j);
  for (let s: i64 = 0; s < j % 16; s++) {}
  return x.v;
}

// Runs between the finish of waiter j's two promises: waits (a while at most) for the waiter.
async function tick(j: i64, c: Promise<i64>): Promise<void> {
  await c;
  for (let s = 0; s < 1000000 && READY.load() < j; s++) {}
}

async function round(r: i64): Promise<i64> {
  READY.store(-1);
  const waiters: Promise<i64>[] = [];
  const ticks: Promise<void>[] = [];
  const firsts: ((v: Val) => void)[] = [];
  const seconds: ((v: Val) => void)[] = [];
  const thirds: ((v: i64) => void)[] = [];
  for (let j: i64 = 0; j < 600; j++) {
    const a = Promise.withResolvers<Val>();
    const b = Promise.withResolvers<Val>();
    const c = Promise.withResolvers<i64>();
    firsts.push(a.resolve);
    seconds.push(b.resolve);
    thirds.push(c.resolve);
    ticks.push(tick(j, c.promise));
    waiters.push(spawn(waiter(j, a.promise, b.promise)));
  }
  await yieldNow();
  const m = spawn(marker());
  for (let j = 0; j < 600; j++) {
    const f = firsts[j];
    f(new Val(j));
    const c = thirds[j];
    c(j);
    const s = seconds[j];
    s(new Val(j));
  }
  while (MARKED.load() <= r) {}
  await m;
  await Promise.all(ticks);
  let sum: i64 = 0;
  for (const v of await Promise.all(waiters)) {
    sum += v;
  }
  return sum;
}

async function main() {
  let sum: i64 = 0;
  for (let r = 0; r < 4; r++) {
    sum += await round(r);
    await sleep(40);
  }
  console.log(`waited ${sum}, marked ${MARKED.load()}`);
}
"#;

/// Runs of [`SETTLED_ACROSS`] at each worker count.
const SETTLE_RUNS: usize = 5;

/// A task on worker 1 settles `main`'s promise with an object that holds a promise of its own,
/// and `main` hands the object on to a third task the moment it is settled. The settling step
/// (std/lang/promise/settle.velt `fulfillLater`) used to open the promise's latch while its
/// frame still referenced the object: handed on in that window the object was copied, the copy
/// shared the promise inside on `main`'s task, and the frame's own reference was then dropped
/// on worker 1, a holder of a shared promise dropped off its task (an `ICE:`, #3360). The window
/// is a few instructions, so `main`'s std passes this nearly always on a quiet machine (1 run
/// in 30 failed on a loaded one); with the window widened (the settling step spinning 0.3 ms
/// before its frame ends) `main`'s std fails every run and the fix none.
#[test]
fn a_value_settled_from_another_task_is_let_go_before_its_awaiter_sees_it() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "settled_across", SETTLED_ACROSS);
    let mut bad = Vec::new();
    for threads in ["4", "8"] {
        for i in 0..SETTLE_RUNS {
            let (code, out, err) = run(&exe, threads);
            if code != 0 || out != "done 600\n" || !all_freed(&err) {
                bad.push(format!(
                    "{threads} workers, run {i}: exit {code}, stdout {out:?}, stderr {err}"
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} runs went wrong:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The program of [`a_value_settled_from_another_task_is_let_go_before_its_awaiter_sees_it`].
const SETTLED_ACROSS: &str = r#"class Holder {
  p: Promise<number>;
  constructor(p: Promise<number>) {
    this.p = p;
  }
}

// Finishes after `n` turns of its task.
async function soon(n: i64): Promise<number> {
  for (let i: i64 = 0; i < n; i++) {
    await yieldNow();
  }
  return 1;
}

// On worker 1: settles main's promise with an object that holds a promise of this task, and
// returns. The settling step runs at the task's next poll.
async function maker(r: i64, give: (h: Holder) => void): Promise<number> {
  const p = soon(1 + (r % 4));
  give(new Holder(p));
  return 1;
}

// On worker 2: awaits the promise in what it was given.
async function user(h: Holder): Promise<number> {
  return await h.p;
}

async function main() {
  let sum = 0;
  for (let r: i64 = 0; r < 300; r++) {
    const got = Promise.withResolvers<Holder>();
    sum += await spawn(maker(r, got.resolve), { worker: 1 });
    // Handed on the moment it is settled: main is awake, and the task on worker 1 may still be
    // in the step that settled it.
    sum += await spawn(user(await got.promise), { worker: 2 });
  }
  console.log(`done ${sum}`);
}
"#;

/// Runs of [`BARE_PROMISE`] and [`RECORD_PROMISE`] at each worker count. Both failed every run
/// before the fix, so a few runs are enough.
const BARE_RUNS: usize = 3;

/// A task on worker 1 settles `main`'s promise with a bare promise of its own task, and with an
/// object type holding one. `needsTransfer` said a promise has nothing to transfer, so `resolve`
/// settled at once and copied the transferred promise into the outcome on worker 1: the copy of
/// a holder handed over shares its proxy there, and `main` dropped a handle of a hub of worker 1's
/// task (an `ICE:` every run, #3389). The settling step's own copy after `transfer` did the same
/// once `needsTransfer` was right.
#[test]
fn a_promise_settled_from_another_task_reaches_its_awaiter_handed_over() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let mut bad = Vec::new();
    for (name, src) in [
        ("bare_promise", BARE_PROMISE),
        ("record_promise", RECORD_PROMISE),
    ] {
        let exe = build_std(dir.path(), name, src);
        for threads in ["4", "8"] {
            for i in 0..BARE_RUNS {
                let (code, out, err) = run(&exe, threads);
                if code != 0 || out != "done 40\n" || !all_freed(&err) {
                    bad.push(format!(
                        "{name}, {threads} workers, run {i}: exit {code}, stdout {out:?}, stderr {err}"
                    ));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} runs went wrong:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The bare promise of [`a_promise_settled_from_another_task_reaches_its_awaiter_handed_over`]
/// (#3369's review, `p2h_bare_promise_temp`).
const BARE_PROMISE: &str = r#"async function soon(n: i64): Promise<number> {
  for (let i: i64 = 0; i < n; i++) {
    await yieldNow();
  }
  return 1;
}

// On worker 1: settles main's promise with a promise of this task, made for the call.
async function maker(give: (p: Promise<number>) => void): Promise<number> {
  give(soon(2));
  for (let y: i64 = 0; y < 6; y++) {
    await yieldNow();
  }
  return 1;
}

async function main() {
  let sum = 0;
  for (let r: i64 = 0; r < 20; r++) {
    const got = Promise.withResolvers<Promise<number>>();
    const mk = spawn(maker(got.resolve), { worker: 1 });
    const p = await got.promise;
    sum += await p;
    sum += await mk;
  }
  console.log(`done ${sum}`);
}
"#;

/// The object type holding a promise of
/// [`a_promise_settled_from_another_task_reaches_its_awaiter_handed_over`] (#3369's review,
/// `p6_record`): `main` hands it on to a task on worker 2.
const RECORD_PROMISE: &str = r#"type R = { p: Promise<number> };

async function soon(n: i64): Promise<number> {
  for (let i: i64 = 0; i < n; i++) {
    await yieldNow();
  }
  return 1;
}

// On worker 1: settles main's promise with a record holding a promise of this task.
async function maker(give: (r: R) => void): Promise<number> {
  give({ p: soon(2) });
  for (let y: i64 = 0; y < 6; y++) {
    await yieldNow();
  }
  return 1;
}

// On worker 2: awaits the promise in what it was given.
async function user(r: R): Promise<number> {
  return await r.p;
}

async function main() {
  let sum = 0;
  for (let r: i64 = 0; r < 20; r++) {
    const got = Promise.withResolvers<R>();
    const mk = spawn(maker(got.resolve), { worker: 1 });
    sum += await spawn(user(await got.promise), { worker: 2 });
    sum += await mk;
  }
  console.log(`done ${sum}`);
}
"#;
