//! A promise handed to another task under the executor in Velt (#160, #2854 step 1;
//! std/lang/hooks/sched_node_transfer.velt): its result is transferred on the task that produces
//! it, so an object that task still uses reaches the other task as a copy. A promise awaited at
//! several places is shared by a hub in Velt (#212, #2854 step 2; sched_share.velt), whose
//! holders resume in Node's order and, handed to another task, await a copy made on the
//! promise's task (sched_share_hand.velt). Each program is built
//! with `VELT_SCHEDULER=std`, prints what Node prints, and runs with the debug runtime's block
//! counters (`VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1`) with one worker and with four: every block
//! is freed, none is used after it was freed.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `src` built with `VELT_SCHEDULER=std`, then run with `VELT_THREADS=1`: its stdout.
fn build_and_run(dir: &Path, name: &str, src: &str) -> String {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(exe_name(name));
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
    let o = command(&exe)
        .env("VELT_THREADS", "1")
        .env_remove("VELT_SCHEDULER")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    assert!(
        o.status.success(),
        "{name}: {out}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    out
}

fn exe_name(name: &str) -> String {
    format!("{name}{}", if cfg!(windows) { ".exe" } else { "" })
}

/// Builds and runs `src`, which prints `want`; then `runs` times with one worker and with four,
/// counted: it prints `want` and frees every block.
fn prints_counted(name: &str, src: &str, want: &str, runs: usize) {
    // A `VELT_SCHEDULER=std` program builds on unix only (no reactor in Velt on Windows yet).
    if !cfg!(unix) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    assert_eq!(build_and_run(dir.path(), name, src), want, "{name}");
    counted_runs(dir.path(), name, want, runs);
}

/// The std build of `name` in `dir` run `runs` times with one worker and with four, with the
/// debug runtime's block counters: it prints `want` and frees every block.
fn counted_runs(dir: &Path, name: &str, want: &str, runs: usize) {
    let exe = dir.join(exe_name(name));
    for threads in ["1", "4"] {
        for run in 0..runs {
            let o = command(&exe)
                .env("VELT_THREADS", threads)
                .env("VELT_RT_DEBUG_ALLOC", "1")
                .env("VELT_RC_STATS", "1")
                .env_remove("VELT_SCHEDULER")
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&o.stderr);
            let at = format!("{name}, {threads} worker(s), run {run}");
            assert!(o.status.success(), "{at}: {stderr}");
            assert_eq!(
                String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
                want,
                "{at}"
            );
            let Some(stats) = stderr.lines().rev().find(|l| l.starts_with("rc stats:")) else {
                // Only the debug runtime counts (a `--release` test run links the release one).
                if cfg!(debug_assertions) {
                    panic!("{at}: no rc stats in:\n{stderr}");
                }
                return;
            };
            let blocks = stats
                .split_whitespace()
                .find_map(|w| w.strip_prefix("blocks="))
                .and_then(|b| b.split_once('/'))
                .unwrap_or_else(|| panic!("{at}: no block counts in {stats}"));
            assert_eq!(blocks.0, blocks.1, "{at}: blocks left unfreed ({stats})");
        }
    }
}

/// The golden covers a started call, a settled `new Promise`, `Promise.all`, a kept
/// `Promise.race` and a join handle: under std a node keeps the glue and runs it as it finishes,
/// and a race is boxed in a node of its task, since its children are not the runtime's to mark.
#[test]
fn a_promise_handed_to_a_task_delivers_a_copy_there() {
    prints_counted(
        "spawn_promise_result_copied",
        include_str!("../../../tests/golden/lang/spawn_promise_result_copied.velt"),
        include_str!("../../../tests/golden/lang/spawn_promise_result_copied.out"),
        3,
    );
}

/// A race spawned as it is built (`spawn(Promise.race(ps))`, `Promise.any`, a race that can
/// reject): under std the race is boxed and started on the spawning task, which copies the
/// winner's result there.
#[test]
fn a_spawned_race_delivers_a_copy_to_its_task() {
    prints_counted(
        "spawned_race_copied",
        SPAWNED_RACE,
        "race: main 100000 task 0 same false\n\
         any: main 100000 task 0 same false\n\
         rejecting race: main 100000 task 0 same false\n",
        3,
    );
}

/// The producing task's worker copies a large result (300,000 objects) while the task it was
/// handed to, on another worker, polls the promise: the copy runs before the promise shows
/// finished there. Run the other way round, that task took the original as the copy released it
/// (a use after free, which the debug runtime's block checks stop at once).
#[test]
fn a_result_is_copied_before_the_other_task_can_take_it() {
    prints_counted("copied_before_taken", COPIED_BEFORE_TAKEN, "shared 0\n", 2);
}

/// A promise awaited at several places is shared (#212): under std by a hub in Velt
/// (sched_share.velt), which queues the waiting holders in the task's microtask queue in the
/// order they started waiting as the promise settles, as Node queues its reactions. The golden's
/// callers resume between the settle handlers of two shared promises in Node's order.
#[test]
fn shared_promise_waiters_resume_in_node_order() {
    prints_counted(
        "promise_shared_inflight",
        include_str!("../../../tests/golden/lang/promise_shared_inflight.velt"),
        include_str!("../../../tests/golden/lang/promise_shared_inflight.out"),
        3,
    );
}

/// Holders of one shared promise handed to tasks (a handle, and in every other round the
/// original itself, while main keeps a handle): each task awaits a copy made on the promise's
/// task (sched_share_hand.velt), main the original object, and at four workers the tasks run
/// on other threads than the promise's.
#[test]
fn holders_of_a_shared_promise_handed_to_tasks_get_copies() {
    prints_counted(
        "shared_holders_handed_over",
        SHARED_HOLDERS,
        "main holds the original 20 tasks got copies 40\n",
        3,
    );
}

/// `Promise.race` over two promises that settle up to three microtask ticks apart, each
/// shared or not: the winner is the one Node picks (the first to settle; for promises settled
/// already, the first in the array). The output is Node's (v24); the runtime's executor differs
/// in six rounds (`plain 2/1` prints `a`), which goes away with it (#2649), so only the executor
/// in Velt runs it.
#[test]
fn a_race_over_shared_and_unshared_promises_picks_node_s_winner() {
    prints_counted(
        "race_shared_order",
        RACE_SHARED_ORDER,
        RACE_SHARED_ORDER_OUT,
        2,
    );
}

/// The program of [`a_spawned_race_delivers_a_copy_to_its_task`].
const SPAWNED_RACE: &str = r#"class Obj {
  n: i64 = 0;
}
async function give(o: Obj): Promise<Obj> {
  return o;
}
async function giveOrFail(o: Obj, fail: bool): Promise<Obj> {
  if (fail) {
    throw new Error("lost");
  }
  return o;
}
async function count(o: Obj): Promise<i64> {
  for (let i = 0; i < 100000; i++) {
    o.n += 1;
  }
  return o.n;
}
async function main() {
  const a = new Obj();
  const w = spawn(Promise.race([give(a), give(a)]));
  const c = await count(a);
  const got = await w;
  console.log("race: main", c, "task", got.n, "same", got === a);
  const b = new Obj();
  const x = spawn(Promise.any([giveOrFail(b, true), giveOrFail(b, false)]));
  const d = await count(b);
  const got2 = await x;
  console.log("any: main", d, "task", got2.n, "same", got2 === b);
  const e = new Obj();
  const y = spawn(Promise.race([giveOrFail(e, false), giveOrFail(e, true)]));
  const f = await count(e);
  try {
    const got3 = await y;
    console.log("rejecting race: main", f, "task", got3.n, "same", got3 === e);
  } catch (err) {
    console.log("rejecting race threw", (err as Error).message);
  }
}
"#;

/// The program of [`a_result_is_copied_before_the_other_task_can_take_it`]: each round the task
/// waits a little longer before it awaits, so some round polls during the copy. A task that got
/// the original instead of a copy shows in main's object, and in release builds too; the use
/// after free, at four workers, shows through the debug runtime's block checks.
const COPIED_BEFORE_TAKEN: &str = r#"class Item {
  n: i64 = 0;
}
class Big {
  items: Item[] = [];
}
// timing: the spins open the window this test is about: a task polls the promise while its
// producer copies the result.
function spin(ms: number): void {
  const s = Date.now();
  while (Date.now() - s < ms) {}
}
async function make(b: Big, ms: i64): Promise<Big> {
  await sleep(ms);
  return b;
}
// The task bumps the object it got: a copy, so the original (main's) stays at 0.
async function take(p: Promise<Big>, ms: number): Promise<i64> {
  spin(ms);
  const b = await p;
  b.items[0].n += 1;
  return b.items[0].n;
}
async function main() {
  const o = new Big();
  for (let i = 0; i < 300000; i++) {
    o.items.push(new Item());
  }
  let shared = 0;
  for (let j = 0; j < 30; j++) {
    const p = make(o, 20);
    const t = spawn(take(p, 10 + j * 1.5));
    spin(5);
    if ((await t) !== 1 || o.items[0].n !== 0) {
      shared += 1;
    }
  }
  console.log("shared", shared);
}
"#;

/// The program of [`holders_of_a_shared_promise_handed_to_tasks_get_copies`].
const SHARED_HOLDERS: &str = r#"class Item {
  n: i64 = 0;
}
class Big {
  items: Item[] = [];
}
async function make(o: Big): Promise<Big> {
  await sleep(5);
  return o;
}
// Each task bumps the object it got: a copy of its own, so it sees 1, and main's stays at 0.
async function take(p: Promise<Big>): Promise<i64> {
  const b = await p;
  b.items[0].n += 1;
  return b.items[0].n;
}
async function main() {
  let mains = 0;
  let copies = 0;
  for (let round = 0; round < 20; round++) {
    const o = new Big();
    for (let i = 0; i < 2000; i++) {
      o.items.push(new Item());
    }
    const p = make(o);
    const q = p;
    const r = p;
    let mine: Big;
    let a: Promise<i64>;
    let b: Promise<i64>;
    if (round % 2 === 0) {
      a = spawn(take(q));
      b = spawn(take(r));
      mine = await p;
    } else {
      // The original goes to a task; main keeps a handle.
      a = spawn(take(p));
      b = spawn(take(r));
      mine = await q;
    }
    for (const n of [await a, await b]) {
      if (n === 1) {
        copies += 1;
      }
    }
    if (mine === o && o.items[0].n === 0) {
      mains += 1;
    }
  }
  console.log("main holds the original", mains, "tasks got copies", copies);
}
"#;

/// The program of [`a_race_over_shared_and_unshared_promises_picks_node_s_winner`].
const RACE_SHARED_ORDER: &str = r#"async function value(tag: string, ticks: number): Promise<string> {
  for (let i = 0; i < ticks; i++) {
    await Promise.resolve();
  }
  return tag;
}
async function round(name: string, ta: number, tb: number, shareA: boolean, shareB: boolean) {
  const a = value("a", ta);
  const b = value("b", tb);
  const keepA = shareA ? a : null;
  const keepB = shareB ? b : null;
  const w = await Promise.race([a, b]);
  console.log(name, w, keepA !== null, keepB !== null);
  if (keepA) await keepA;
  if (keepB) await keepB;
}
async function main() {
  for (const [ta, tb] of [[1, 2], [2, 1], [1, 1], [3, 1], [1, 3], [0, 2], [2, 0]]) {
    await round(`plain ${ta}/${tb}`, ta, tb, false, false);
    await round(`A shared ${ta}/${tb}`, ta, tb, true, false);
    await round(`B shared ${ta}/${tb}`, ta, tb, false, true);
    await round(`both ${ta}/${tb}`, ta, tb, true, true);
  }
}
main();
"#;

/// What Node prints for [`RACE_SHARED_ORDER`].
const RACE_SHARED_ORDER_OUT: &str = "plain 1/2 a false false\n\
A shared 1/2 a true false\n\
B shared 1/2 a false true\n\
both 1/2 a true true\n\
plain 2/1 b false false\n\
A shared 2/1 b true false\n\
B shared 2/1 b false true\n\
both 2/1 b true true\n\
plain 1/1 a false false\n\
A shared 1/1 a true false\n\
B shared 1/1 a false true\n\
both 1/1 a true true\n\
plain 3/1 b false false\n\
A shared 3/1 b true false\n\
B shared 3/1 b false true\n\
both 3/1 b true true\n\
plain 1/3 a false false\n\
A shared 1/3 a true false\n\
B shared 1/3 a false true\n\
both 1/3 a true true\n\
plain 0/2 a false false\n\
A shared 0/2 a true false\n\
B shared 0/2 a false true\n\
both 0/2 a true true\n\
plain 2/0 b false false\n\
A shared 2/0 b true false\n\
B shared 2/0 b false true\n\
both 2/0 b true true\n\
";

/// A program that runs nothing on the executor doesn't load it (velt_sema `lazy_std`
/// `sched_hooks`): a promise it copies (a struct's field, an array's elements) is the runtime's,
/// shared by the runtime's hub. Lowering asked for `schedShare` there and stopped with "std
/// without the hook" (bench/jsx, whose elements hold promises).
#[test]
fn a_program_without_tasks_shares_its_promises_without_the_executor() {
    let src = "struct S {\n  p: Promise<number> | undefined;\n}\n\nclass Box {\n  pending: Promise<number>[] = [];\n}\n\nfunction main() {\n  const a: S = { p: undefined };\n  const b = a;\n  const box = new Box();\n  const xs = box.pending.slice();\n  console.log(b.p === undefined, xs.length);\n}\n";
    prints_counted("share_without_tasks", src, "true 0\n", 1);
}
