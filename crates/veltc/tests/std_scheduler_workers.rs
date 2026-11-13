//! The executor in Velt's workers (std/lang/hooks/sched.velt, #2649 step 3): a program compiled
//! with `VELT_SCHEDULER=std` starts `VELT_THREADS` workers (threads started through `@export`
//! and `pthread_create`), a pinned spawn runs on the worker it names, wakes cross workers through
//! the home worker's inbox and reactor, module state keeps one lock across them, and the program
//! ends once every worker is idle. Each program is built with the variable, runs with
//! `VELT_THREADS=4` and must print what Node prints. The tests with sockets are in
//! std_scheduler_workers_net.rs.

use std::path::Path;

use no_window::command;
use sched_workers_support::{build_and_run, build_and_run_with, has_threads, manifest, prints};

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod std_unix;
mod test_dir;

/// The borrowers' take, white-box (std_unix/lang/hooks/sched_spread.test.velt): on a worker
/// block and tasks built by hand, the tasks that may not move stay where they were, in their
/// order, and the worker is left alone only when the older half held tasks and all of them
/// stayed. It imports the scheduler's modules, which only link on macOS and Linux.
#[cfg(unix)]
#[test]
fn sched_spread_white_box_tests_pass() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    std_unix::velt_test_in_std_copy(
        "lang/hooks/sched_spread.test.velt",
        include_str!("std_unix/lang/hooks/sched_spread.test.velt"),
    );
}

#[test]
fn pinned_tasks_run_on_their_worker_and_wake_across_workers() {
    if !has_threads() {
        return;
    }
    prints(
        "pinned",
        r#"import { workerCount, currentWorker } from "velt/task";
import { channel, Channel, HandOff } from "velt/sync";

async function pong(inbox: Channel<i64>, out: Channel<i64>): Promise<i64> {
  let seen = 0;
  while (true) {
    const v = await inbox.receive();
    if (v == null || v < 0) {
      return seen;
    }
    seen++;
    await out.send(v + 1);
  }
}

async function square(n: i64): Promise<i64> {
  await yieldNow();
  return n * n;
}

async function waiter(h: shared<HandOff<i64>>): Promise<i64> {
  await h.park();
  return h.claim() ?? -1;
}

async function main() {
  const n = workerCount();
  console.log(`workers: ${n}`);
  let right = 0;
  for (let w = 0; w < n; w++) {
    const got = await spawn((async () => currentWorker())(), { worker: w });
    if (got == w) right++;
  }
  console.log(`pinned where asked: ${right == n}`);
  const a = channel<i64>();
  const b = channel<i64>();
  const p = spawn(pong(a, b), { worker: n - 1 });
  let v = 0;
  for (let i = 0; i < 2000; i++) {
    await a.send(v);
    v = (await b.receive()) ?? 0;
  }
  await a.send(-1);
  console.log(`pingpong ${v} ${await p}`);
  const hs: Promise<i64>[] = [];
  for (let i = 0; i < 2000; i++) {
    hs.push(spawn(square(i), { worker: i % n }));
  }
  let sum = 0;
  for (const h of hs) sum += await h;
  console.log(`sum ${sum}`);
  let got = 0;
  for (let i = 0; i < 200; i++) {
    const h = shared(new HandOff<i64>());
    const t = spawn(waiter(h), { worker: i % n });
    await yieldNow();
    h.give(i);
    got += await t;
  }
  console.log(`handoff ${got}`);
}
"#,
        "workers: 4\npinned where asked: true\npingpong 2000 2000\nsum 2664667000\nhandoff 19900\n",
    );
}

#[test]
fn module_state_is_one_lock_across_workers() {
    if !has_threads() {
        return;
    }
    // Each task reads, yields and writes the counter in separate polls only through `await`; a
    // read and its write in one poll happen under the module lock, so no increment is lost.
    prints(
        "module_lock",
        r#"import { workerCount } from "velt/task";

let counter = 0;
const names: string[] = [];

async function bump(w: i64, n: i64): Promise<void> {
  for (let i = 0; i < n; i++) {
    const was = counter;
    counter = was + 1;
    if (i % 100 == 0) {
      await yieldNow();
    }
  }
  names.push(`w${w}`);
}

async function main() {
  const tasks: Promise<void>[] = [];
  for (let w = 0; w < workerCount(); w++) {
    tasks.push(spawn(bump(w, 20000), { worker: w }));
  }
  for (const t of tasks) await t;
  console.log(`counter ${counter}, ${names.length} tasks`);
}
"#,
        "counter 80000, 4 tasks\n",
    );
}

#[test]
fn the_program_ends_once_every_worker_is_idle() {
    if !has_threads() {
        return;
    }
    // A detached pinned task that sleeps keeps the process alive after `main`; one waiting on a
    // promise nobody settles does not. The late task starts its sleep only once `main` printed
    // (a channel gates it), so the order is by signals, not by time.
    prints(
        "lifetime",
        r#"import { channel, Channel } from "velt/sync";

async function late(gate: Channel<i64>): Promise<void> {
  await gate.receive();
  await sleep(30);
  console.log("late task done");
}

async function stuck(): Promise<void> {
  const { promise } = Promise.withResolvers<void>();
  await promise;
  console.log("never");
}

async function main() {
  const gate = channel<i64>();
  spawn(late(gate), { worker: 3 });
  spawn(stuck(), { worker: 2 });
  console.log("main done");
  gate.trySend(1);
}
"#,
        "main done\nlate task done\n",
    );
}

/// A pinned task's result that a promise it started goes on using reaches the joiner as a copy
/// of its own (the result's transfer glue runs as the task finishes, as the runtime's does):
/// without it both threads share the object and its count.
#[test]
fn a_pinned_tasks_result_is_transferred_to_its_joiner() {
    if !has_threads() {
        return;
    }
    prints(
        "transfer",
        r#"// A pinned task's result that a promise it started keeps using: the joiner gets its own copy.
class Obj {
  n: i64 = 0;
}

const DONE = new Atomic<i64>(0);

function churn(o: Obj): i64 {
  const held: Obj[] = [];
  for (let i = 0; i < 20; i++) {
    held.push(o);
  }
  return held.length as i64;
}

async function keepUsing(o: Obj): Promise<void> {
  for (let i = 0; i < 200; i++) {
    churn(o);
    o.n += 1;
    await yieldNow();
  }
  DONE.add(1);
}

async function work(): Promise<Obj> {
  const o = new Obj();
  const p = keepUsing(o);
  p.catch(() => {});
  await yieldNow();
  return o;
}

async function main() {
  let copies: i64 = 0;
  for (let k = 0; k < 20; k++) {
    const o = await spawn(work(), { worker: 1 });
    while (DONE.load() <= k) {
      churn(o);
      await yieldNow();
    }
    // The promise went on with the task's object, not this one.
    if (o.n < 200) {
      copies++;
    }
  }
  console.log(`copies ${copies}`);
}
"#,
        "copies 20\n",
    );
}

/// An `unref()`'d interval set by a task on worker 1 asks the process whether it ends (not
/// worker 1's block, which has no probe): it ticks while `main` waits for its second tick (a
/// channel, not a time, orders the lines; a timer keeps the process alive meanwhile), and the
/// program ends with the interval still set.
#[test]
fn an_unrefd_timer_on_another_worker_ticks_while_main_sleeps() {
    if !has_threads() {
        return;
    }
    prints(
        "unref",
        r#"import { channel, Channel } from "velt/sync";

async function ticker(ticked: Channel<i64>): Promise<void> {
  let n = 0;
  const t = setInterval(() => {
    n++;
    if (n <= 2) {
      console.log(`tick ${n}`);
    }
    if (n == 2) {
      ticked.trySend(n);
    }
  }, 5);
  t.unref();
}

async function main() {
  const ticked = channel<i64>(1);
  // Keeps the process alive while `main` waits: the interval does not.
  const keep = setTimeout(() => {}, 1000000);
  await spawn(ticker(ticked), { worker: 1 });
  await ticked.receive();
  clearTimeout(keep);
  console.log("main done");
}
"#,
        "tick 1\ntick 2\nmain done\n",
    );
}

/// `main`'s last line, printed on another worker (the stall monitor moved `main` off a worker 0
/// that the OS took off its core), reaches the OS before worker 0 ends the process: stdout is
/// buffered per thread. On Linux, 40 runs at 4 workers with the program and two spinning threads
/// on two cores (`sched_setaffinity`), so worker 0 is often off its core while `main` waits
/// behind its yields: without the fix 5 runs in 40 printed nothing (Linux arm64 VM). Elsewhere
/// (macOS cannot pin threads) 10 plain runs.
#[test]
fn mains_last_line_printed_on_another_worker_is_written() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(dir.path(), "last", LAST_LINE, "4");
    assert_eq!((code, out.as_str()), (0, "sum 200000\n"), "last line");
    let lost = contended_runs(dir.path(), "last", "4", "sum 200000\n");
    assert!(lost.is_empty(), "{}", lost.join("\n"));
}

/// A task other than `main` that finishes last on another worker: what it printed reaches the OS
/// before worker 0, woken by the end of the last live task, ends the process. `main` returns at
/// once; a task pinned to worker 1 yields 20,000 times, then prints. Runs as
/// [`mains_last_line_printed_on_another_worker_is_written`] does: without the fix (`finish`
/// writing out the thread's output), the line was missing in 30 runs of 40 on two contended cores
/// (review of #2706).
#[test]
fn the_last_line_of_a_task_ending_last_on_another_worker_is_written() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(dir.path(), "late", LATE_LINE, "4");
    assert_eq!((code, out.as_str()), (0, "late 20000\n"), "late line");
    let lost = contended_runs(dir.path(), "late", "4", "late 20000\n");
    assert!(lost.is_empty(), "{}", lost.join("\n"));
}

/// The program of [`the_last_line_of_a_task_ending_last_on_another_worker_is_written`].
const LATE_LINE: &str = r#"async function late(): Promise<void> {
  let n: i64 = 0;
  for (let i = 0; i < 20000; i++) {
    await yieldNow();
    n += 1;
  }
  console.log(`late ${n}`);
}

async function main() {
  spawn(late(), { worker: 1 });
}
"#;

/// Runs the std build of `name` (built in `dir`) with `threads` workers: on Linux 40 times with the
/// program and two spinning threads on cores 0 and 1 (`sched_setaffinity`), elsewhere 10 plain
/// runs (macOS cannot pin threads). The runs that did not print `want` with exit code 0.
fn contended_runs(dir: &Path, name: &str, threads: &str, want: &str) -> Vec<String> {
    let exe = dir.join(format!(
        "{name}_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let linux = cfg!(target_os = "linux");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let burners: Vec<_> = (0..if linux { 2 } else { 0 })
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                pin_to_two_cores();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
        })
        .collect();
    let mut lost = Vec::new();
    for run in 0..if linux { 40 } else { 10 } {
        let mut cmd = command(&exe);
        cmd.env("VELT_THREADS", threads)
            .env_remove("VELT_SCHEDULER");
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: only an affinity call between fork and exec.
            unsafe {
                cmd.pre_exec(|| {
                    pin_to_two_cores();
                    Ok(())
                });
            }
        }
        let o = cmd.output().unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        if (o.status.code(), out.as_ref()) != (Some(0), want) {
            lost.push(format!("run {run}: {:?} {out:?}", o.status.code()));
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for b in burners {
        b.join().unwrap();
    }
    lost
}

/// Pins the calling thread (or a child between fork and exec) to cores 0 and 1 (Linux only).
fn pin_to_two_cores() {
    #[cfg(target_os = "linux")]
    // SAFETY: a zeroed `cpu_set_t` filled by the libc macros, passed with its size.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(0, &mut set);
        libc::CPU_SET(1, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

/// The program of [`mains_last_line_printed_on_another_worker_is_written`].
const LAST_LINE: &str = r#"async function main() {
  let sum: i64 = 0;
  for (let i = 0; i < 200000; i++) {
    await yieldNow();
    sum += 1;
  }
  console.log(`sum ${sum}`);
}
"#;

/// A token relayed between three pinned workers while `main` only joins, 30 runs of the std
/// build at 8 workers: the end detection must never end the program while a worker has a task
/// (a worker read as idle and woken before the scan ends sends worker 0 back to look again).
/// A stress run, not a guard: it passes with that rescan removed too (the window is too narrow
/// to reach on purpose); the rescan's argument is in sched_end.velt `quiet`.
#[test]
fn the_end_waits_for_tasks_woken_during_the_scan() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(dir.path(), "ring", RING, "8");
    assert_eq!(
        (code, out.as_str()),
        (0, "relayed 303\n"),
        "ring, the executor in Velt"
    );
    let exe = dir.path().join(format!(
        "ring_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    for run in 0..30 {
        let o = command(&exe)
            .env("VELT_THREADS", "8")
            .env_remove("VELT_SCHEDULER")
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        assert_eq!(
            (o.status.code(), out.as_ref()),
            (Some(0), "relayed 303\n"),
            "ring, run {run}"
        );
    }
}

/// The relay program of [`the_end_waits_for_tasks_woken_during_the_scan`].
const RING: &str = r#"import { channel, Channel } from "velt/sync";

// Passes the token on: each relay is pinned to its own worker; main only joins, so the program
// ends only through the workers' end detection.
async function relay(inbox: Channel<i64>, out: Channel<i64>, last: bool): Promise<i64> {
  let seen: i64 = 0;
  while (true) {
    const v = await inbox.receive();
    if (v == null) {
      out.close();
      return seen;
    }
    seen++;
    if (last && v >= 300) {
      out.close();
      return seen;
    }
    await out.send(v + 1);
  }
}

async function main() {
  const a = channel<i64>();
  const b = channel<i64>();
  const c = channel<i64>();
  const r1 = spawn(relay(a, b, false), { worker: 1 });
  const r2 = spawn(relay(b, c, false), { worker: 2 });
  const r3 = spawn(relay(c, a, true), { worker: 3 });
  await a.send(0);
  const seen = (await r1) + (await r2) + (await r3);
  console.log(`relayed ${seen}`);
}
"#;

/// One worker with the stall rescue (`VELT_THREADS=1 VELT_RESCUE=1`): the rescue of a stalled
/// poll is taken back once the spares are idle (std/lang/hooks/sched_relax.velt), and given again
/// at the next stall. `main` computes until the reader it spawned has started, which only a spare
/// can run; the reader then waits on its socket (registered with the spare's reactor) while `main`
/// sleeps, long enough for the spare to retire. The answer that wakes the reader then runs it on
/// worker 0; with the rescue one-way it ran on the spare (`on worker 0 false`). A second stall moves
/// a task to a spare again. (`currentWorker()` names a spare as a configured worker: the program
/// tells worker 0's thread by a thread-local.)
#[test]
fn the_rescue_of_the_only_worker_is_taken_back_once_the_spares_are_idle() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run_with(dir.path(), "relax", RELAX, "1", &[("VELT_RESCUE", "1")]);
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "moved true, read hi on worker 0 true\nmoved again true\n"
        ),
        "the executor in Velt, 1 worker"
    );
}

/// The program of [`the_rescue_of_the_only_worker_is_taken_back_once_the_spares_are_idle`].
const RELAX: &str = r#"import { listen, connect, TcpStream } from "velt/net";

// 1 on worker 0's thread (`main` sets it): a spare is a thread of its own.
const ON_MAIN = new ThreadLocal<i64>(0);
const STARTED = new Atomic<i64>(0);
const AGAIN = new Atomic<i64>(0);

async function reader(s: TcpStream): Promise<string> {
  const first = ON_MAIN.get();
  STARTED.store(1);
  const got = await s.readString();
  return `moved ${first === 0}, read ${got} on worker 0 ${ON_MAIN.get() === 1}`;
}

async function mark(): Promise<i64> {
  AGAIN.store(1);
  return ON_MAIN.get();
}

async function main() {
  ON_MAIN.set(1);
  const l = await listen("127.0.0.1:0");
  const c = await connect(`127.0.0.1:${l.port}`);
  const s = await l.accept();
  const r = spawn(reader(s));
  // Busy until a spare has started the reader: the monitor's rescue of this poll.
  while (STARTED.load() === 0) {}
  // Idle: the spare parks, the reader waiting on its socket; the monitor takes the rescue back.
  await sleep(300);
  await c.write("hi");
  console.log(await r);
  const m = spawn(mark());
  while (AGAIN.load() === 0) {}
  console.log(`moved again ${(await m) === 0}`);
  c.close();
  l.close();
}
"#;

/// One worker with the stall rescue (`VELT_THREADS=1 VELT_RESCUE=1`): `main` spawns 200k
/// tasks, then computes in the same poll until a spare has run some of them. The monitor moves
/// half of what the poll spawned at each tick and puts the newer half back; when the poll ended
/// while it did, worker 0 had taken its shared mode on and joined that stack already, so the half
/// put back was never run and the end scan, finding nothing to do, ended the program with exit
/// code 0 and no output (4 runs in 5 in the Linux arm64 VM, debug and release alike, while the
/// monitor could take all 200k; now it takes from the first 64 a poll spawns, and the window is
/// much narrower: a guard of the rescue's end more than of that race).
/// The limit that comes with it: the monitor moves only the first 64 tasks a poll spawns (the
/// rest are on a list the poll links without atomics, std/lang/hooks/sched_task.velt `E_PLIST`),
/// so a poll that spawns more and then waits without a pause for all of them never ends (as on
/// one worker without the monitor); this program waits for 16.
#[test]
fn tasks_put_back_while_worker_0_leaves_its_lazy_mode_still_run() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run_with(
        dir.path(),
        "put_back",
        PUT_BACK,
        "1",
        &[("VELT_RESCUE", "1")],
    );
    assert_eq!((code, out.as_str()), (0, "sum 599994\n"), "run 0");
    let exe = dir.path().join(format!(
        "put_back_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    for run in 1..5 {
        let o = command(&exe)
            .env("VELT_THREADS", "1")
            .env_remove("VELT_SCHEDULER")
            .env("VELT_RESCUE", "1")
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        assert_eq!(
            (o.status.code(), out.as_ref()),
            (Some(0), "sum 599994\n"),
            "run {run}"
        );
    }
}

/// The program of [`tasks_put_back_while_worker_0_leaves_its_lazy_mode_still_run`].
const PUT_BACK: &str = r#"class Count {
  done: Atomic<i64> = new Atomic<i64>(0);
}

async function work(c: shared<Count>, i: i64): Promise<i64> {
  c.done.add(1);
  await yieldNow();
  return i % 7;
}

async function main() {
  const c = shared(new Count());
  const hs: Promise<i64>[] = [];
  for (let i = 0; i < 200000; i++) {
    hs.push(spawn(work(c, i)));
  }
  // Busy until a spare has run some of them: the monitor's rescue of this poll.
  while (c.done.load() < 16) {}
  let sum: i64 = 0;
  for (const h of hs) {
    sum += await h;
  }
  console.log(`sum ${sum}`);
}
"#;
