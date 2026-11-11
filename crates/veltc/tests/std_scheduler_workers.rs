//! The executor in Velt's workers (std/lang/hooks/sched.velt, #2649 step 3): a program compiled
//! with `VELT_SCHEDULER=std` starts `VELT_THREADS` workers (threads started through `@export`
//! and `pthread_create`), a pinned spawn runs on the worker it names, wakes cross workers through
//! the home worker's inbox and reactor, module state keeps one lock across them, and the program
//! ends once every worker is idle. Each program is built with and without the variable, both
//! binaries run with `VELT_THREADS=4` and must print the same.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `src` built (with `VELT_SCHEDULER=std` when `std`), run with `threads` workers: its exit
/// code and stdout.
fn build_and_run(dir: &Path, name: &str, src: &str, std: bool, threads: &str) -> (i32, String) {
    build_and_run_with(dir, name, src, std, threads, &[])
}

/// [`build_and_run`] with more variables (`env`) for the run.
fn build_and_run_with(
    dir: &Path,
    name: &str,
    src: &str,
    std: bool,
    threads: &str,
    env: &[(&str, &str)],
) -> (i32, String) {
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
        .env("VELT_THREADS", threads)
        .env_remove("VELT_SCHEDULER")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    let err = String::from_utf8_lossy(&o.stderr);
    (o.status.code().unwrap_or(-1), format!("{out}{err}"))
}

/// Builds and runs `src` both ways with 4 workers (and the std build with 1 too): each prints
/// `want` and exits with 0.
fn same_on_both(name: &str, src: &str, want: &str) {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, rt) = build_and_run(dir.path(), name, src, false, "4");
    assert_eq!(
        (code, rt.as_str()),
        (0, want),
        "{name}: the runtime's executor"
    );
    let (code, std) = build_and_run(dir.path(), name, src, true, "4");
    assert_eq!(
        (code, std.as_str()),
        (0, want),
        "{name}: the executor in Velt, 4 workers"
    );
}

/// Workers only start where `#sched-thread` has threads (macOS and Linux for now).
fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

#[test]
fn pinned_tasks_run_on_their_worker_and_wake_across_workers() {
    if !has_threads() {
        return;
    }
    same_on_both(
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
    same_on_both(
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
    same_on_both(
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

#[test]
fn a_socket_serves_from_another_worker() {
    if !has_threads() {
        return;
    }
    same_on_both(
        "socket",
        r#"import { listen, connect } from "velt/net";
import { channel, Channel } from "velt/sync";
import { currentWorker } from "velt/task";

async function serve(ports: Channel<i64>): Promise<string> {
  const l = await listen("127.0.0.1:0");
  await ports.send(l.port as i64);
  const s = await l.accept();
  const got = await s.readString();
  await s.write(`echo ${got} from ${currentWorker()}`);
  s.close();
  l.close();
  return got;
}

async function main() {
  const ports = channel<i64>();
  const server = spawn(serve(ports), { worker: 2 });
  const port = (await ports.receive()) ?? 0;
  const c = await connect(`127.0.0.1:${port}`);
  await c.write("hi");
  c.shutdown();
  console.log(await c.readString());
  c.close();
  console.log(`server got ${await server}`);
}
"#,
        "echo hi from 2\nserver got hi\n",
    );
}

/// A connection served on one worker moves to another mid-stream (its stream handed to a task
/// pinned there): it keeps serving, its registration follows the task to that worker's reactor
/// (#2370), and the client runs on a third worker.
#[test]
fn a_connection_moves_to_another_worker_and_keeps_serving() {
    if !has_threads() {
        return;
    }
    same_on_both(
        "moving",
        r#"import { listen, connect, TcpStream } from "velt/net";
import { channel, Channel } from "velt/sync";
import { currentWorker } from "velt/task";

async function answer(s: TcpStream, from: i64, n: i64): Promise<void> {
  for (let i = from; i < from + n; i++) {
    const got = await s.readString();
    await s.write(`${got} -> pong ${i} on ${currentWorker()}`);
  }
}

async function serveRest(s: TcpStream): Promise<void> {
  await answer(s, 3, 3);
  s.close();
}

async function serve(ports: Channel<i64>): Promise<void> {
  const l = await listen("127.0.0.1:0");
  await ports.send(l.port as i64);
  for (let k = 0; k < 2; k++) {
    const s = await l.accept();
    await answer(s, 0, 3);
    await spawn(serveRest(s), { worker: 2 });
  }
  l.close();
}

async function client(port: i64, k: i64): Promise<string[]> {
  const c = await connect(`127.0.0.1:${port}`);
  const out: string[] = [];
  for (let i = 0; i < 6; i++) {
    await c.write(`ping ${k}.${i}`);
    out.push(await c.readString());
  }
  c.close();
  return out;
}

async function main() {
  const ports = channel<i64>();
  const server = spawn(serve(ports), { worker: 1 });
  const port = (await ports.receive()) ?? 0;
  for (let k = 0; k < 2; k++) {
    for (const line of await spawn(client(port, k), { worker: 3 })) {
      console.log(line);
    }
  }
  await server;
}
"#,
        "ping 0.0 -> pong 0 on 1\nping 0.1 -> pong 1 on 1\nping 0.2 -> pong 2 on 1\n\
         ping 0.3 -> pong 3 on 2\nping 0.4 -> pong 4 on 2\nping 0.5 -> pong 5 on 2\n\
         ping 1.0 -> pong 0 on 1\nping 1.1 -> pong 1 on 1\nping 1.2 -> pong 2 on 1\n\
         ping 1.3 -> pong 3 on 2\nping 1.4 -> pong 4 on 2\nping 1.5 -> pong 5 on 2\n",
    );
}

/// A server task waiting in `accept` on a worker that then runs on without a pause: the stall
/// monitor turns that worker's reactor for it and moves the woken server to another worker,
/// where it accepts, reads and answers (its new connections registered there); with the rescue
/// off (`VELT_RESCUE=0`) the clients would wait for ever.
#[test]
fn a_server_moved_off_a_stalled_worker_keeps_serving() {
    if !has_threads() {
        return;
    }
    same_on_both(
        "rescue",
        r#"import { listen, connect, TcpListener } from "velt/net";

const DONE = new Atomic<i64>(0);

async function serve(l: TcpListener, n: i64): Promise<i64> {
  let served: i64 = 0;
  for (let k = 0; k < n; k++) {
    const s = await l.accept();
    const got = await s.readString();
    await s.write(`echo ${got}`);
    s.close();
    served++;
  }
  l.close();
  return served;
}

async function client(port: i64, k: i64): Promise<string> {
  const c = await connect(`127.0.0.1:${port}`);
  await c.write(`c${k}`);
  c.shutdown();
  const got = await c.readString();
  c.close();
  DONE.add(1);
  return got;
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const port = l.port as i64;
  const server = spawn(serve(l, 3));
  // The server starts and waits in accept, its listener on this worker's reactor.
  await yieldNow();
  const clients: Promise<string>[] = [];
  for (let k = 0; k < 3; k++) {
    clients.push(spawn(client(port, k), { worker: 1 }));
  }
  // This worker runs on without a pause until every client has its answer: only a server moved
  // to another worker can give it.
  while (DONE.load() < 3) {}
  for (const line of await Promise.all(clients)) {
    console.log(line);
  }
  console.log(`served ${await server}`);
}
"#,
        "echo c0\necho c1\necho c2\nserved 3\n",
    );
}

/// A listener opened by `main` accepts and reads in a task pinned to another worker: the handle
/// resolves in the process's table, and the connection's registration follows the task.
#[test]
fn a_listener_opened_on_worker_0_accepts_on_another() {
    if !has_threads() {
        return;
    }
    same_on_both(
        "listener",
        r#"import { listen, connect, TcpListener } from "velt/net";

async function serve(l: TcpListener): Promise<string> {
  const s = await l.accept();
  const got = await s.readString();
  await s.write(`echo ${got}`);
  s.close();
  return got;
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const port = l.port;
  const server = spawn(serve(l), { worker: 1 });
  const c = await connect(`127.0.0.1:${port}`);
  await c.write("hi");
  c.shutdown();
  console.log(await c.readString());
  c.close();
  console.log(`server ${await server}`);
  l.close();
}
"#,
        "echo hi\nserver hi\n",
    );
}

/// A socket that a second task uses (a pool's connection, which tasks take turns with) stays on
/// its reactor, and the task follows it: here the task, spawned from worker 2, uses the
/// connection `main` used last, and its read's reply wakes it on the worker whose reactor has the
/// connection, not across threads. That worker is `main`'s at its last read (the read moves the
/// connection to `main`'s home, whose reactor wakes it), which the spread may have made another
/// than worker 0 on a loaded machine, so the program prints whether the task ended up there, not
/// a worker's index. The stall monitor is off (`VELT_RESCUE=0`). With `join` removed (the task
/// stays on worker 2) it prints `false` (60 of 60 runs on two contended cores, Linux arm64 VM; 60 of 60 `true` with it).
/// The server answers the task 20 ms after its request, so the answer wakes the task: one there
/// already when the task reads would leave it where it is.
#[test]
fn a_task_follows_a_socket_tasks_take_turns_with() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run_with(
        dir.path(),
        "follows",
        r#"import { listen, connect, TcpStream } from "velt/net";
import { channel, Channel } from "velt/sync";
import { currentWorker } from "velt/task";

async function echo(ports: Channel<i64>): Promise<void> {
  const l = await listen("127.0.0.1:0");
  await ports.send(l.port as i64);
  const s = await l.accept();
  for (let i = 0; i < 2; i++) {
    const got = await s.readString();
    if (i == 1) {
      // The reply wakes `user` (it waits for it by then), rather than being there already.
      await sleep(20);
    }
    await s.write(`${got}!`);
  }
  s.close();
  l.close();
}

async function user(c: TcpStream, socketWorker: i64): Promise<string> {
  await c.write("b");
  const got = await c.readString();
  const here = currentWorker();
  c.close();
  return `${got} on the connection's worker: ${here == socketWorker}`;
}

async function starter(c: TcpStream, socketWorker: i64): Promise<string> {
  return await spawn(user(c, socketWorker));
}

async function main() {
  const ports = channel<i64>();
  const server = spawn(echo(ports), { worker: 1 });
  const port = (await ports.receive()) ?? 0;
  // A sleep anchors `main` to the worker it runs on (it may run on worker 1, woken there by the
  // send): its connection then lives on that worker's reactor, the one `currentWorker()` names.
  await sleep(1);
  const c = await connect(`127.0.0.1:${port}`);
  await c.write("a");
  console.log(await c.readString());
  const socketWorker = currentWorker();
  console.log(await spawn(starter(c, socketWorker), { worker: 2 }));
  await server;
}
"#,
        true,
        "4",
        &[("VELT_RESCUE", "0")],
    );
    assert_eq!(
        (code, out.as_str()),
        (0, "a!\nb! on the connection's worker: true\n")
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
    same_on_both(
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
    same_on_both(
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
    let (code, out) = build_and_run(dir.path(), "last", LAST_LINE, true, "4");
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
    let (code, out) = build_and_run(dir.path(), "late", LATE_LINE, true, "4");
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
    let (code, out) = build_and_run(dir.path(), "ring", RING, true, "8");
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

/// One worker with the stall rescue (`VELT_THREADS=1`, the default rescue): `main` spawns 200k
/// tasks, then computes in the same poll until a spare has run some of them. The monitor moves
/// half of what the poll spawned at each tick and puts the newer half back; when the poll ended
/// while it did, worker 0 had taken its shared mode on and joined that stack already, so the half
/// put back was never run and the end scan, finding nothing to do, ended the program with exit
/// code 0 and no output (4 runs in 5 in the Linux arm64 VM, debug and release alike, while the
/// monitor could take all 200k; now it takes from the first 64 a poll spawns, and the window is
/// much narrower: a guard of the rescue's end more than of that race).
#[test]
fn tasks_put_back_while_worker_0_leaves_its_lazy_mode_still_run() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(dir.path(), "put_back", PUT_BACK, true, "1");
    assert_eq!((code, out.as_str()), (0, "sum 599994\n"), "run 0");
    let exe = dir.path().join(format!(
        "put_back_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    for run in 1..5 {
        let o = command(&exe)
            .env("VELT_THREADS", "1")
            .env_remove("VELT_SCHEDULER")
            .env_remove("VELT_RESCUE")
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
