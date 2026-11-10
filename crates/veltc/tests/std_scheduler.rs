//! The executor in Velt (std/lang/hooks/sched.velt, #2649 step 3.1): a program compiled with
//! `VELT_SCHEDULER=std` runs its root, its spawned tasks and `yieldNow` there, and the runtime's
//! leaves (a sleep on the runtime's tokio thread, channels) wake its tasks through their C
//! waker. Each program is built with and without the variable, both binaries run with
//! `VELT_THREADS=1` and must print the same; the one built with it holds the executor's code,
//! the other doesn't.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `src` built (with `VELT_SCHEDULER=std` when `std`) and run, which must exit with `code`: its
/// stdout, and its binary.
fn build_and_run(dir: &Path, name: &str, src: &str, std: bool, code: i32) -> (String, Vec<u8>) {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!(
        "{name}{}{}",
        if std { "_std" } else { "" },
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let mut build = command(env!("CARGO_BIN_EXE_velt"));
    build
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env_remove("VELT_SCHEDULER");
    if std {
        build.env("VELT_SCHEDULER", "std");
    }
    let o = build.output().unwrap();
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
    assert_eq!(
        o.status.code(),
        Some(code),
        "{name} (std: {std}): {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    (out, std::fs::read(&exe).unwrap())
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Whether a `VELT_SCHEDULER=std` program builds here: the executor in Velt waits in an epoll or
/// kqueue reactor, and Windows has no reactor in Velt yet (#2649 step 4), so its build would not
/// link. The tests compare the two executors only where both exist.
fn std_builds_here() -> bool {
    cfg!(unix)
}

/// Builds and runs `src` both ways: each prints `want`, and only the std build has the executor.
fn same_on_both(name: &str, src: &str, want: &str) {
    same_on_both_exiting(name, src, want, 0);
}

/// [`same_on_both`] for a program that exits with `code`.
fn same_on_both_exiting(name: &str, src: &str, want: &str, code: i32) {
    if !std_builds_here() {
        return;
    }
    let dir = test_dir::TestDir::new();
    same_on_both_in(dir.path(), name, src, want, code);
}

/// [`same_on_both`], then the std build run again with the debug runtime's block counters
/// (`VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1`): every block allocated through the runtime must be
/// freed by the end. The programs return objects from their tasks, which the counters see
/// wherever the executor takes its task blocks from: a join handle never dropped keeps its
/// task's result (an unclaimed one in the task's slot, a claimed one as the share the handle's
/// copies hold).
fn same_on_both_freeing_all(name: &str, src: &str, want: &str) {
    if !std_builds_here() {
        return;
    }
    let dir = test_dir::TestDir::new();
    same_on_both_in(dir.path(), name, src, want, 0);
    let exe = dir.path().join(format!(
        "{name}_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let o = command(&exe)
        .env("VELT_THREADS", "1")
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RC_STATS", "1")
        .env_remove("VELT_SCHEDULER")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert!(o.status.success(), "{name} (counted): {stderr}");
    let Some(stats) = stderr.lines().rev().find(|l| l.starts_with("rc stats:")) else {
        // Only the debug runtime counts (a `--release` test run links the release one).
        if cfg!(debug_assertions) {
            panic!("{name}: no rc stats in:\n{stderr}");
        }
        return;
    };
    let blocks = stats
        .split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))
        .and_then(|b| b.split_once('/'))
        .unwrap_or_else(|| panic!("{name}: no block counts in {stats}"));
    assert_eq!(blocks.0, blocks.1, "{name}: blocks left unfreed ({stats})");
}

fn same_on_both_in(dir: &Path, name: &str, src: &str, want: &str, code: i32) {
    runtime_support::build_native_runtime(manifest());
    let (rt, rt_bin) = build_and_run(dir, name, src, false, code);
    let (std, std_bin) = build_and_run(dir, name, src, true, code);
    assert_eq!(rt, want, "{name}: the runtime's executor");
    assert_eq!(std, want, "{name}: the executor in Velt");
    assert!(
        contains(&std_bin, b"schedBlockOn"),
        "{name}: no executor in Velt"
    );
    assert!(
        !contains(&rt_bin, b"schedBlockOn"),
        "{name}: the executor in Velt without the variable"
    );
}

#[test]
fn spawned_tasks_yield_and_join() {
    same_on_both(
        "spawn_yield",
        "async function work(i: i64): Promise<i64> {
  console.log(`work ${i} starts`);
  await yieldNow();
  console.log(`work ${i} resumes`);
  return i * 2;
}

async function main() {
  const a = spawn(work(1));
  const b = spawn(work(2));
  console.log(\"spawned\");
  console.log(await a + await b);
}
",
        "spawned\nwork 1 starts\nwork 2 starts\nwork 1 resumes\nwork 2 resumes\n6\n",
    );
}

/// A file read finishes on the runtime's blocking threads: its wake reaches the worker waiting in
/// the reactor (sched_task.velt `schedule`). And sleeps of spawned tasks (std's own timers under
/// the executor in Velt) resume by deadline.
#[test]
fn a_leaf_on_another_thread_wakes_the_task() {
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("wake.txt");
    let file = file.to_string_lossy().replace('\\', "/");
    let src = format!(
        "import {{ readFile, writeFile }} from \"velt/fs\";

async function nap(ms: i64, s: string): Promise<string> {{
  await sleep(ms);
  return s;
}}

async function main() {{
  await writeFile(\"{file}\", \"from another thread\");
  console.log(await readFile(\"{file}\"));
  const slow = spawn(nap(60, \"slow\"));
  const fast = spawn(nap(5, \"fast\"));
  console.log(await fast);
  console.log(await slow);
}}
"
    );
    same_on_both_in(
        dir.path(),
        "leaf_wake",
        &src,
        "from another thread\nfast\nslow\n",
        0,
    );
}

#[test]
fn channels_hand_values_between_tasks() {
    same_on_both(
        "channel_rounds",
        "import { channel, Channel } from \"velt/sync\";

async function echo(inbox: Channel<i64>, out: Channel<i64>): Promise<void> {
  while (true) {
    const v = await inbox.receive();
    if (v == null) {
      return;
    }
    out.trySend(v * 10);
  }
}

async function main() {
  const a = channel<i64>();
  const b = channel<i64>();
  const t = spawn(echo(a, b));
  let sum: i64 = 0;
  for (let i: i64 = 0; i < 1000; i++) {
    a.trySend(i);
    sum += (await b.receive()) ?? 0;
  }
  a.close();
  await t;
  console.log(sum);
}
",
        "4995000\n",
    );
}

#[test]
fn results_nobody_claims_are_dropped_and_tasks_outlive_main() {
    // A detached task's string result and a dropped handle's are dropped (the debug runtime
    // reports a leak otherwise); a task still sleeping when `main` returns runs to its end.
    same_on_both(
        "unclaimed",
        "async function text(n: i64): Promise<string> {
  await yieldNow();
  return `text ${n}`;
}

async function late(): Promise<void> {
  await sleep(20);
  console.log(\"late task done\");
}

async function main() {
  spawn(text(1));
  const h = spawn(text(2));
  spawn(late());
  const all = await Promise.all([spawn(text(3)), spawn(text(4))]);
  console.log(all.join(\", \"));
  console.log(\"main done\");
}
",
        "text 3, text 4\nmain done\nlate task done\n",
    );
}

#[test]
fn a_dropped_spawn_handle_frees_its_task() {
    // The join handle of `spawn(f())` is the task's last reference once it is done: dropped at
    // once (a statement) or at the end of its block (a stored handle never awaited), the task's
    // block and the object it returned are freed.
    same_on_both_freeing_all(
        "spawn_dropped",
        "class Tag {
  n: i64;
  constructor(n: i64) {
    this.n = n;
  }
}

async function tag(n: i64): Promise<Tag> {
  await yieldNow();
  return new Tag(n);
}

async function main() {
  spawn(tag(1));
  {
    const h = spawn(tag(2));
  }
  await yieldNow();
  await yieldNow();
  console.log(\"done\");
}
",
        "done\n",
    );
}

#[test]
fn an_awaited_spawn_handle_frees_its_task() {
    // Awaiting a join handle, stored or not, consumes it, fulfilled or rejected (caught): the
    // task's block and the object it returned are freed.
    same_on_both_freeing_all(
        "spawn_awaited",
        "class Failed extends Error {}

class Tag {
  n: i64;
  constructor(n: i64) {
    this.n = n;
  }
}

async function tag(n: i64): Promise<Tag> {
  await yieldNow();
  return new Tag(n);
}

async function fail(): Promise<Tag> throws Failed {
  await yieldNow();
  throw new Failed(\"failed\");
}

async function main() {
  const h = spawn(tag(1));
  console.log((await h).n);
  console.log((await spawn(tag(2))).n);
  try {
    await spawn(fail());
  } catch (e) {
    console.log(\"caught\", e.message);
  }
}
",
        "1\n2\ncaught failed\n",
    );
}

#[test]
fn raced_spawn_handles_free_their_tasks() {
    // `Promise.race` takes the handles: the winner's and the loser's tasks are both freed (the
    // loser runs to its end after `main`).
    same_on_both_freeing_all(
        "spawn_raced",
        "class Tag {
  n: i64;
  constructor(n: i64) {
    this.n = n;
  }
}

async function now(): Promise<Tag> {
  return new Tag(1);
}

async function later(): Promise<Tag> {
  await yieldNow();
  await yieldNow();
  await yieldNow();
  return new Tag(2);
}

async function main() {
  console.log((await Promise.race([spawn(now()), spawn(later())])).n);
}
",
        "1\n",
    );
}

#[test]
fn a_combinator_s_spawned_losers_reject_quietly() {
    // The handles a rejected `Promise.all` leaves behind (inline: given up to the task) and the
    // losers of a `Promise.race` over a stored array are handled: their tasks reject later,
    // after the handles were dropped, without an uncaught report, and are freed.
    same_on_both_freeing_all(
        "spawn_losers",
        "import { Channel, channel } from \"velt/sync\";

class Failed extends Error {}

async function failNow(): Promise<i64> {
  throw new Failed(\"now\");
}

async function failLater(go: Channel<i64>): Promise<i64> {
  await go.receive();
  throw new Failed(\"later\");
}

async function value(): Promise<i64> {
  return 5;
}

async function main() {
  const go = channel<i64>();
  try {
    await Promise.all([spawn(failNow()), spawn(failLater(go))]);
  } catch (e) {
    console.log(\"all:\", e.message);
  }
  const racers: Promise<i64, Failed>[] = [spawn(value()), spawn(failLater(go))];
  console.log(\"race:\", await Promise.race(racers));
  go.close();
  await yieldNow();
  await yieldNow();
  console.log(\"done\");
}
",
        "all: now\nrace: 5\ndone\n",
    );
}

#[test]
fn a_spawn_handle_dropped_after_its_task_rejected_reports_it() {
    // The handle dropped after the task rejected reports the rejection as uncaught and exits
    // with 1 (#148); a handle never dropped would leave it unreported until the guard sleep,
    // which must never end, ends.
    same_on_both_exiting(
        "spawn_dropped_after_reject",
        "class Failed extends Error {}

async function fail(): Promise<i64> throws Failed {
  throw new Failed(\"lost\");
}

async function main() {
  {
    const h = spawn(fail());
    await yieldNow();
    await yieldNow();
    console.log(\"dropping the handle\");
  }
  await sleep(2000);
  console.log(\"unreachable\");
}
",
        "dropping the handle\n",
        1,
    );
}

#[test]
fn started_promises_run_at_once_and_microtasks_keep_node_order() {
    // A stored promise runs until its first `await` when it is created; `await` steps, `then`
    // and `queueMicrotask` take turns in one queue; a `resolve` called later queues its waiter.
    same_on_both(
        "started_micro",
        "async function f(n: i64): Promise<i64> {
  console.log(`f${n} start`);
  await yieldNow();
  console.log(`f${n} resumed`);
  return n * 10;
}

async function g(tag: string): Promise<void> {
  console.log(tag, 1);
  await null;
  console.log(tag, 2);
  await Promise.resolve();
  console.log(tag, 3);
}

async function main() {
  const p = f(1);
  const q = f(2);
  console.log(\"after starts\");
  console.log(await p + await q);
  const { promise, resolve } = Promise.withResolvers<i64>();
  const w = promise.then((v) => console.log(\"then\", v));
  const gx = g(\"x\");
  queueMicrotask(() => console.log(\"qm\"));
  resolve(5);
  const gy = g(\"y\");
  console.log(\"sync end\");
  await w;
  await gx;
  await gy;
}
",
        "f1 start\nf2 start\nafter starts\nf1 resumed\nf2 resumed\n30\nx 1\ny 1\nsync end\nx 2\nqm\nthen 5\ny 2\nx 3\ny 3\n",
    );
}

#[test]
fn a_combinators_losers_are_handled_and_an_exit_code_ends_at_once() {
    // A rejected promise handed to `Promise.all` is handled by it (no uncaught report after the
    // rejection is caught); `main` returning 3 does not wait for the sleeping loser.
    same_on_both_exiting(
        "handled_exit",
        "class Failed extends Error {}

async function fail(ms: i64): Promise<i64> throws Failed {
  await sleep(ms);
  throw new Failed(`failed after ${ms}`);
}

async function slow(): Promise<i64> {
  await sleep(3000);
  console.log(\"the loser finished\");
  return 1;
}

async function main(): Promise<i32> {
  try {
    await Promise.all([fail(5), fail(30)]);
  } catch (e) {
    console.log(\"caught\", e.message);
  }
  const p = slow();
  console.log(\"returning\");
  return 3;
}
",
        "caught failed after 5\nreturning\n",
        3,
    );
}

/// Promises run from a microtask while they wait on their task's woken stack, and woken again
/// there: the golden that crashed (a node pushed on the stack twice made a cycle) prints Node's
/// order on both executors.
#[test]
fn promises_woken_while_queued_keep_their_task_s_woken_stack_whole() {
    same_on_both(
        "race_settled_order",
        include_str!("../../../tests/golden/lang/promise_race_settled_order.velt"),
        include_str!("../../../tests/golden/lang/promise_race_settled_order.out"),
    );
}

/// A rejected promise nobody handles that finishes in a microtask while it also waits on its
/// task's woken stack goes on the task's list of unhandled rejections through a link of its own:
/// sharing the stack's link cut the stack there, and the promises below it (a timer's callback)
/// kept their queued flag without being on it, so they never ran again and the program hung once
/// `main` was done (3 of 10 runs of the golden). Run 20 times; each must end within 20 seconds.
#[test]
fn a_rejection_finishing_while_woken_keeps_the_woken_stack_whole() {
    if !std_builds_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let src = include_str!("../../../tests/golden/lang/promise_combinators_kept_array.velt");
    let want = include_str!("../../../tests/golden/lang/promise_combinators_kept_array.out");
    let (out, _) = build_and_run(dir.path(), "kept_array", src, true, 0);
    assert_eq!(out, want);
    let exe = dir.path().join(format!(
        "kept_array_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    for run in 0..20 {
        let mut child = command(&exe)
            .env("VELT_THREADS", "1")
            .env_remove("VELT_SCHEDULER")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let id = child.id();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let status = child.wait();
            let _ = tx.send(());
            status
        });
        if rx.recv_timeout(std::time::Duration::from_secs(20)).is_err() {
            // The waiting thread owns the child: kill it by its id.
            let _ = command(if cfg!(windows) { "taskkill" } else { "kill" })
                .args(if cfg!(windows) {
                    vec!["/F".to_string(), "/PID".to_string(), id.to_string()]
                } else {
                    vec!["-9".to_string(), id.to_string()]
                })
                .status();
            let _ = waiter.join();
            panic!("run {run} of the golden did not end (parked with work left)");
        }
        let status = waiter.join().unwrap().unwrap();
        assert!(status.success(), "run {run}: {status}");
    }
}

/// A promise shared with a spawned task, which awaits it while microtasks are queued on both
/// tasks: the holder handed over stays one of the worker's (the executor's tasks have no runtime
/// task, so the runtime took each for code outside a task and gave the holder to the outside
/// home's proxy, which nothing drove: the spawned task waited forever).
#[test]
fn a_spawned_task_awaits_a_shared_promise_while_microtasks_are_queued() {
    same_on_both(
        "shared_spawned",
        "async function work(): Promise<number> {
           await yieldNow();
           return 7;
         }
         async function user(p: Promise<number>, tag: string): Promise<number> {
           queueMicrotask(() => console.log(`${tag}: microtask`));
           const v = await p;
           console.log(`${tag}: got ${v}`);
           return v;
         }
         async function main() {
           const p = work();
           const q = p;
           const a = spawn(user(q, \"spawned\"));
           Promise.resolve().then(() => console.log(\"main: then\"));
           queueMicrotask(() => console.log(\"main: microtask\"));
           const v = await p;
           console.log(`main: got ${v}, spawned returned ${await a}`);
         }",
        "main: then\nmain: microtask\nspawned: microtask\nspawned: got 7\nmain: got 7, spawned returned 7\n",
    );
}

/// WASI has no thread to wake the worker and no OS wait to park it in: `VELT_SCHEDULER=std`
/// does nothing there, and `main`'s root stays the runtime's.
#[test]
fn the_variable_does_nothing_on_wasi() {
    let dir = test_dir::TestDir::new();
    std::fs::write(
        dir.path().join("main.velt"),
        "async function main() {\n  await yieldNow();\n  console.log(\"ok\");\n}\n",
    )
    .unwrap();
    let ir = |std: bool, target: &str| {
        let mut c = command(env!("CARGO_BIN_EXE_velt"));
        c.args(["build", "main.velt", "--emit", "llvm", "--target", target])
            .current_dir(dir.path())
            .env_remove("VELT_SCHEDULER");
        if std {
            c.env("VELT_SCHEDULER", "std");
        }
        let o = c.output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    assert!(!ir(true, "wasm32-wasip1").contains("schedBlockOn"));
    assert!(ir(true, "aarch64-unknown-linux-gnu").contains("schedBlockOn"));
}

/// A promise nobody can settle keeps nothing alive, as in Node and the runtime (quiet.rs): once
/// the worker would park with no keep-alive reference, no pending I/O and no runtime leaf
/// outstanding, the program ends with code 0 (stdout flushed), or with Node's 13 when the top
/// level waits at an `await` (sched_end.velt).
#[test]
fn a_never_settled_promise_exits_like_the_runtime() {
    same_on_both(
        "never_settled",
        "async function stuck(): Promise<void> {
  await new Promise<void>((r) => {});
  console.log(\"never\");
}

async function main() {
  spawn(stuck());
  await sleep(10);
  console.log(\"waiting\");
  await new Promise<void>((r) => {});
  console.log(\"never\");
}
",
        "waiting\n",
    );
    same_on_both_exiting(
        "never_settled_top",
        "console.log(\"before\");
await sleep(10);
const { promise } = Promise.withResolvers<void>();
await promise;
console.log(\"never\");
",
        "before\n",
        13,
    );
}
