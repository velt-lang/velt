//! The I/O budget of a task poll in the executor in Velt (std/lang/hooks/sched_budget.velt,
//! #2815): a connection whose peer always has sent more completes every read without a wait, and
//! used to keep its poll going for as long as that lasted, holding back the rest of its task (a
//! server that drives every connection from one task, as http-server's `acceptAll`) and the rest
//! of its worker (a server task pinned beside it). Each program streams into one connection from
//! one worker for three seconds while another worker sends a ping on a second connection; the ping
//! must be answered while the stream still runs. Built with `VELT_SCHEDULER=std`, run with
//! `VELT_THREADS=4`.

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
    // A watchdog, not a synchronization: the stream ends after three seconds either way.
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

/// The clients and the two connections' servers; `SERVE` (each program's) accepts both.
const COMMON: &str = r#"import { listen, connect, TcpListener, TcpStream } from "velt/net";
import { channel, Channel } from "velt/sync";

// Reads in small pieces until the end, as a parser does: with a peer that writes faster, every
// read finds bytes.
async function drain(s: TcpStream): Promise<void> {
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

async function pong(s: TcpStream): Promise<void> {
  const got = await s.readString();
  await s.write(`${got} -> pong`);
  s.close();
}

async function stream(port: i64, started: Channel<i64>): Promise<i64> {
  const c = await connect(`127.0.0.1:${port}`);
  const chunk: u8[] = Buffer.alloc(65536);
  const until = Date.now() + 3000;
  let first = true;
  while (Date.now() < until) {
    await c.writeBytes(chunk);
    if (first) {
      first = false;
      await started.send(1);
    }
  }
  c.close();
  return Date.now() as i64;
}

async function ping(port: i64): Promise<i64> {
  const c = await connect(`127.0.0.1:${port}`);
  await c.write("ping");
  c.shutdown();
  const got = await c.readString();
  c.close();
  return got == "ping -> pong" ? Date.now() as i64 : -1;
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const port = l.port as i64;
  const server = spawn(serve(l), { worker: 0 });
  const started = channel<i64>();
  const hot = spawn(stream(port, started), { worker: 1 });
  await started.receive();
  const answered = await spawn(ping(port), { worker: 2 });
  const end = await hot;
  await server;
  console.log(`answered while the other connection streamed: ${answered > 0 && answered < end}`);
}
"#;

const WANT: &str = "answered while the other connection streamed: true\n";

/// Both connections are promises of the server's one task (http-server's `acceptAll`): the
/// streaming one's reads spend its own budget, then it waits behind the accept and the ping.
#[test]
fn a_connection_that_always_has_input_lets_the_other_promises_of_its_task_run() {
    if !has_threads() {
        return;
    }
    let src = format!(
        "{COMMON}
async function serve(l: TcpListener): Promise<void> {{
  const hot = drain(await l.accept());
  const cold = pong(await l.accept());
  await cold;
  await hot;
  l.close();
}}
"
    );
    assert_eq!(run_std("fair_promises", &src), WANT);
}

/// Each connection a task pinned to the server's worker (http-server's `acceptSpread` gives a
/// worker several): the streaming task's poll ends once its budget is spent, so the accept loop
/// and the ping's task on that worker run (pinned tasks are never moved off it).
#[test]
fn a_connection_that_always_has_input_lets_the_other_tasks_of_its_worker_run() {
    if !has_threads() {
        return;
    }
    let src = format!(
        "{COMMON}
async function serve(l: TcpListener): Promise<void> {{
  const hot = spawn(drain(await l.accept()), {{ worker: 0 }});
  const cold = spawn(pong(await l.accept()), {{ worker: 0 }});
  await cold;
  await hot;
  l.close();
}}
"
    );
    assert_eq!(run_std("fair_tasks", &src), WANT);
}
