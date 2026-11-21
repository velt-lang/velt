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

/// Runs of [`ARG_TO_SPAWNED_CALL`] and [`COPIES_HANDED_OVER`] at each worker count. Both failed every
/// run before the fix, at one worker too.
const COPY_RUNS: usize = 3;

/// A promise passed to an async closure that is called and spawned at once
/// (`spawn((async (held) => await held)(p))`, #3424). The spawn copies the argument for the task
/// and hands the copy over (its holder forwards to a proxy on this task); the callee then keeps a
/// copy of its argument, made on this task after the hand-over. That copy shared the proxy here,
/// a hub of this task, and the spawned task dropped a handle of it: an `ICE:` every run. A copy
/// of a holder handed over from its task is now handed over too. [`COPIES_HANDED_OVER`] has the
/// same with a record, with the caller awaiting the promise as well, a named async function, and
/// one promise passed to two spawned calls.
#[test]
fn a_copy_of_a_promise_handed_over_from_its_task_is_handed_over_too() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let mut bad = Vec::new();
    for (name, src, want) in [
        ("arg_to_spawned_call", ARG_TO_SPAWNED_CALL, "done 190\n"),
        ("copies_handed_over", COPIES_HANDED_OVER, "done 1140\n"),
        ("settled_before", SETTLED_BEFORE, "done 235\n"),
    ] {
        let exe = build_std(dir.path(), name, src);
        for threads in ["1", "4", "8"] {
            for i in 0..COPY_RUNS {
                let (code, out, err) = run(&exe, threads);
                if code != 0 || out != want || !all_freed(&err) {
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

/// Promises already settled when they are passed, for
/// [`a_copy_of_a_promise_handed_over_from_its_task_is_handed_over_too`] (the review of #3426): the
/// hand-over's proxy then finished at once, before the callee's copy, which shared it on this task
/// again. Node prints the same sum. The rejected promise gets a handler before it is passed, as Node
/// needs too: otherwise its rejection is reported as unhandled when it settles.
const SETTLED_BEFORE: &str = r#"// A promise that has settled before it is passed to an async closure that is called and spawned at
// once: resolved, awaited before, settled without being awaited, rejected before (the callee
// catches), and resolved by another task before.
async function soon(n: number): Promise<number> {
  await yieldNow();
  return n;
}

async function fails(n: number): Promise<number> {
  await yieldNow();
  throw new Error(`no ${n}`);
}

async function settle(give: (v: number) => void, n: number): Promise<void> {
  give(n);
}

async function main(): Promise<void> {
  let sum = 0;
  for (let r = 0; r < 10; r++) {
    // Resolved.
    const a = Promise.resolve(r);
    sum += await spawn((async (held: Promise<number>): Promise<number> => await held)(a));
    // Awaited before.
    const b = soon(r);
    sum += await b;
    sum += await spawn((async (held: Promise<number>): Promise<number> => await held)(b));
    // Settled, not awaited.
    const c = soon(r);
    await yieldNow();
    await yieldNow();
    sum += await spawn((async (held: Promise<number>): Promise<number> => await held)(c));
    // Rejected before; the callee catches.
    const d = fails(r);
    d.catch((e: Error): number => 0);
    await yieldNow();
    await yieldNow();
    sum += await spawn(
      (async (held: Promise<number>): Promise<number> => {
        try {
          return await held;
        } catch (e) {
          return 1;
        }
      })(d),
    );
    // Resolved by another task before.
    const got = Promise.withResolvers<number>();
    await spawn(settle(got.resolve, r));
    const e = got.promise;
    sum += await spawn((async (held: Promise<number>): Promise<number> => await held)(e));
  }
  console.log(`done ${sum}`);
}
main();
"#;

/// The program of #3424 for [`a_copy_of_a_promise_handed_over_from_its_task_is_handed_over_too`].
const ARG_TO_SPAWNED_CALL: &str = r#"async function soon(n: number): Promise<number> {
  for (let i = 0; i < 3; i++) {
    await yieldNow();
  }
  return n;
}

async function main(): Promise<void> {
  let sum = 0;
  for (let r = 0; r < 20; r++) {
    const p = soon(r);
    const t = spawn((async (held: Promise<number>): Promise<number> => await held)(p));
    sum += await t;
  }
  console.log(`done ${sum}`);
}
main();
"#;

/// The other shapes of [`a_copy_of_a_promise_handed_over_from_its_task_is_handed_over_too`].
const COPIES_HANDED_OVER: &str = r#"type R = { p: Promise<number> };

async function soon(n: number): Promise<number> {
  for (let i = 0; i < 3; i++) {
    await yieldNow();
  }
  return n;
}

async function named(held: Promise<number>): Promise<number> {
  return await held;
}

async function main(): Promise<void> {
  let sum = 0;
  for (let r = 0; r < 20; r++) {
    // A record holding the promise, passed to a spawned call through a closure value.
    const p = soon(r);
    const t = spawn((async (held: R): Promise<number> => await held.p)({ p: p }));
    sum += await t;
    // The caller awaits the promise too, after handing a copy over.
    const q = soon(r);
    const u = spawn((async (held: Promise<number>): Promise<number> => await held)(q));
    sum += await q;
    sum += await u;
    // A named async function: its arguments are transferred, not copied.
    const s = soon(r);
    sum += await spawn(named(s));
    // The same promise to two spawned calls.
    const w = soon(r);
    const a = spawn((async (held: Promise<number>): Promise<number> => await held)(w));
    const b = spawn((async (held: Promise<number>): Promise<number> => await held)(w));
    sum += await a;
    sum += await b;
  }
  console.log(`done ${sum}`);
}
main();
"#;
