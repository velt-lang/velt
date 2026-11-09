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

/// Builds and runs `src` both ways: each prints `want`, and only the std build has the executor.
fn same_on_both(name: &str, src: &str, want: &str) {
    same_on_both_exiting(name, src, want, 0);
}

/// [`same_on_both`] for a program that exits with `code`.
fn same_on_both_exiting(name: &str, src: &str, want: &str, code: i32) {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (rt, rt_bin) = build_and_run(dir.path(), name, src, false, code);
    let (std, std_bin) = build_and_run(dir.path(), name, src, true, code);
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

#[test]
fn a_sleep_on_another_thread_wakes_the_task() {
    same_on_both(
        "sleep_wake",
        "async function nap(ms: i64, s: string): Promise<string> {
  await sleep(ms);
  return s;
}

async function main() {
  const slow = spawn(nap(60, \"slow\"));
  const fast = spawn(nap(5, \"fast\"));
  console.log(await fast);
  console.log(await slow);
}
",
        "fast\nslow\n",
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
