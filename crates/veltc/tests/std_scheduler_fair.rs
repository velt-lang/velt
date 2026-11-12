//! The I/O budget of a task poll in the executor in Velt (std/lang/hooks/sched_budget.velt,
//! #2815): a connection whose peer always has sent more completes every read without a wait, and
//! used to keep its poll going for as long as that lasted, holding back the rest of its task (a
//! server that drives every connection from one task, as http-server's `acceptAll`) and the rest
//! of its worker (a server task pinned beside it). No clock decides a verdict: a client first
//! writes a backlog that sits in the socket buffers, a ping connects only after it, and the server
//! records whether the ping was answered before its drain reached the end of the backlog (without
//! a budget the drain reads it all in one go). The accept loop's test queues connections before
//! the server accepts any. Built with `VELT_SCHEDULER=std`, run with `VELT_THREADS=4`.

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

/// Workers only start where `#sched-thread` has threads (macOS and Linux for now).
fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// `src` built with `VELT_SCHEDULER=std` and run with 4 workers: its stdout and stderr.
fn run_std(name: &str, src: &str) -> String {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.path().join(name);
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
    // A watchdog, not a synchronization: every program ends once its clients are served.
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            child.kill().unwrap();
            panic!("{name}: still running after a minute");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&o.stderr)
    )
}

/// The backlog's clients and servers; `SERVE` (each program's) accepts both connections.
const COMMON: &str = r#"import { listen, connect, TcpListener, TcpStream } from "velt/net";
import { channel, Channel } from "velt/sync";

// Fits in the loopback socket buffers, so the client's writes finish without the server reading;
// 4096 reads of 16 bytes, far more than one turn's budget.
const BACKLOG: usize = 65536;
// 1 once the drain has read the whole backlog.
const DRAINED = new Atomic<i64>(0);
// 1: the ping was answered before the drain reached the end of the backlog; 2: after.
const ANSWERED = new Atomic<i64>(0);

// Reads in small pieces until the end, as a parser does: every read finds bytes until the end of
// the backlog.
async function drain(s: TcpStream, started: Channel<i64>): Promise<void> {
  await started.send(1);
  const buf: u8[] = Buffer.alloc(16);
  let total: usize = 0;
  while (true) {
    const n = s.tryRead(buf);
    if (n == null) {
      break;
    }
    total += n as usize;
    if (total >= BACKLOG) {
      DRAINED.store(1);
    }
    if (n == 0) {
      await s.readable();
    }
  }
  s.close();
}

async function pong(s: TcpStream): Promise<void> {
  const got = await s.readString();
  ANSWERED.store(DRAINED.load() == 0 ? 1 : 2);
  await s.write(`${got} -> pong`);
  s.close();
}

// Writes the backlog, says so, and holds the connection open until the ping has its answer.
async function hot(port: i64, written: Channel<i64>, done: Channel<i64>): Promise<void> {
  const c = await connect(`127.0.0.1:${port}`);
  await c.writeBytes(Buffer.alloc(BACKLOG));
  await written.send(1);
  await done.receive();
  c.close();
}

// Sends its ping in full before the server starts to read either connection.
async function ping(port: i64, pinged: Channel<i64>): Promise<bool> {
  const c = await connect(`127.0.0.1:${port}`);
  await c.write("ping");
  c.shutdown();
  await pinged.send(1);
  const got = await c.readString();
  c.close();
  return got == "ping -> pong";
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const port = l.port as i64;
  const pinged = channel<i64>();
  const server = spawn(serve(l, pinged), { worker: 0 });
  const written = channel<i64>();
  const done = channel<i64>();
  const h = spawn(hot(port, written, done), { worker: 1 });
  await written.receive();
  const ok = await spawn(ping(port, pinged), { worker: 2 });
  await done.send(1);
  await h;
  await server;
  console.log(`ping answered before the backlog was read: ${ok && ANSWERED.load() == 1}`);
}
"#;

const WANT: &str = "ping answered before the backlog was read: true\n";

/// Both connections are promises of the server's one task (http-server's `acceptAll`): the
/// draining one's reads spend the turn's budget, then it waits behind the ping's promise.
#[test]
fn a_connection_that_always_has_input_lets_the_other_promises_of_its_task_run() {
    if !has_threads() {
        return;
    }
    let src = format!(
        "{COMMON}
async function serve(l: TcpListener, pinged: Channel<i64>): Promise<void> {{
  const a = await l.accept();
  const b = await l.accept();
  await pinged.receive();
  const started = channel<i64>(1);
  const hotDrain = drain(a, started);
  const cold = pong(b);
  await cold;
  await hotDrain;
  l.close();
}}
"
    );
    assert_eq!(run_std("fair_promises", &src), WANT);
}

/// Each connection a task pinned to the server's worker (http-server's `acceptSpread` gives a
/// worker several): the draining task's poll ends once its budget is spent, so the server task
/// and the ping's task, spawned once the drain has started, run on that worker (pinned tasks are
/// never moved off it).
#[test]
fn a_connection_that_always_has_input_lets_the_other_tasks_of_its_worker_run() {
    if !has_threads() {
        return;
    }
    let src = format!(
        "{COMMON}
async function serve(l: TcpListener, pinged: Channel<i64>): Promise<void> {{
  const a = await l.accept();
  const b = await l.accept();
  await pinged.receive();
  const started = channel<i64>(1);
  const hotDrain = spawn(drain(a, started), {{ worker: 0 }});
  await started.receive();
  const cold = spawn(pong(b), {{ worker: 0 }});
  await cold;
  await hotDrain;
  l.close();
}}
"
    );
    assert_eq!(run_std("fair_tasks", &src), WANT);
}

/// An accept loop that starts a promise per connection (http-server's `acceptAll`): a started
/// promise's first run spends its starter's budget rather than refilling it, so the loop's turn
/// ends after 128 socket operations (accepts and the connections' first reads) and a task pinned
/// to the same worker runs before the queued connections are all accepted. 100 connections, as
/// macOS keeps at most 128 waiting; each sends 64 bytes read in 16-byte pieces (600 operations).
#[test]
fn an_accept_loop_that_starts_a_promise_per_connection_lets_the_other_tasks_of_its_worker_run() {
    if !has_threads() {
        return;
    }
    let src = r#"import { listen, connect, TcpListener, TcpStream } from "velt/net";
import { channel, Channel } from "velt/sync";

const N: i64 = 100;
const ACCEPTED = new Atomic<i64>(0);
// The connections accepted when the pinned task ran.
const SEEN = new Atomic<i64>(-1);

async function handle(s: TcpStream): Promise<void> {
  const buf: u8[] = Buffer.alloc(16);
  while (true) {
    const n = s.tryRead(buf);
    if (n == null) {
      break;
    }
    if (n == 0) {
      await s.readable();
    }
  }
  s.close();
}

async function pinned(): Promise<void> {
  SEEN.store(ACCEPTED.load());
}

async function serve(l: TcpListener, ready: Channel<i64>): Promise<void> {
  await ready.receive();
  const other = spawn(pinned(), { worker: 0 });
  const conns: Promise<void>[] = [];
  for (let i: i64 = 0; i < N; i++) {
    const s = await l.accept();
    ACCEPTED.store(i + 1);
    conns.push(handle(s));
  }
  for (const p of conns) {
    await p;
  }
  await other;
  l.close();
}

// Every connection is queued, its bytes sent and its end too, before the server accepts one.
async function clients(port: i64, ready: Channel<i64>): Promise<void> {
  for (let i: i64 = 0; i < N; i++) {
    const c = await connect(`127.0.0.1:${port}`);
    await c.writeBytes(Buffer.alloc(64));
    c.close();
  }
  await ready.send(1);
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const ready = channel<i64>();
  const server = spawn(serve(l, ready), { worker: 0 });
  await spawn(clients(l.port as i64, ready), { worker: 1 });
  await server;
  const seen = SEEN.load();
  console.log(`the pinned task ran before the queued connections were accepted: ${seen >= 0 && seen < N}`);
}
"#;
    assert_eq!(
        run_std("fair_accept", src),
        "the pinned task ran before the queued connections were accepted: true\n"
    );
}
