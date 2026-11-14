//! `fetch`'s plain-TCP connections and their keep-alive pool under the executor in Velt (#2649):
//! sockets on its reactor (std/net/http/fetch/client/dial_sched.velt) pooled in Velt
//! (std/net/fd_pool.velt); `https:` stays on the runtime's `connectTls` and pool. Each program runs
//! with one worker and with four (killed after a minute: a lost wake hangs) and must print the same
//! each time, and its binary must dial in Velt.

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

/// `src` built in `dir`: the binary's path.
fn build(dir: &Path, name: &str, src: &str) -> std::path::PathBuf {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    exe
}

/// Runs `exe` with `threads` workers: its stdout.
fn run(exe: &Path, threads: u32) -> String {
    let mut child = command(exe)
        .env("VELT_THREADS", threads.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A watchdog, not a synchronization: a lost wake would hang the test run.
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            child.kill().unwrap();
            panic!(
                "{} (threads: {threads}): still running after a minute",
                exe.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    assert!(
        o.status.success(),
        "{} (threads: {threads}): {}{}",
        exe.display(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Builds `src` and runs it at one and four workers: each run prints `want`, and the binary dials
/// in Velt (`connectUntil`, std/net/fd_socket.velt).
fn same_everywhere(name: &str, src: &str, want: &str) {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), name, src);
    for threads in [1, 4] {
        assert_eq!(run(&exe, threads), want, "{name}: {threads} workers");
    }
    assert!(
        contains(&std::fs::read(&exe).unwrap(), b"connectUntil"),
        "{name}: fetch dials the runtime"
    );
}

/// A raw HTTP/1.1 server counting its connections: `serve` answers every request on a connection
/// with the number of connections accepted so far, and closes it after each answer when `closing`
/// (else after `/last`).
const SERVER: &str = "import { listen, TcpListener, TcpStream } from \"velt/net\";

const ACCEPTED = new Atomic<i64>(0);
const CLOSED = new Atomic<i64>(0);

async function answer(s: TcpStream, closing: bool) {
  let pending = \"\";
  while (true) {
    let chunk = \"\";
    try {
      chunk = await s.readString();
    } catch (e) {
      break;
    }
    if (chunk == \"\") {
      break;
    }
    pending += chunk;
    let end = pending.indexOf(\"\\r\\n\\r\\n\");
    while (end >= 0) {
      // `/last`: the client's last request on this connection, which then closes (an open one
      // would keep the process alive).
      const last = pending.startsWith(\"GET /last \");
      pending = pending.slice(end + 4);
      const body = `conn ${ACCEPTED.load()}`;
      await s.write(`HTTP/1.1 200 OK\\r\\nContent-Length: ${body.length}\\r\\n\\r\\n${body}`);
      if (closing || last) {
        s.close();
        CLOSED.add(1);
        return;
      }
      end = pending.indexOf(\"\\r\\n\\r\\n\");
    }
  }
  s.close();
}

function serve(l: TcpListener, closing: bool) {
  spawn(async () => {
    while (true) {
      let s: TcpStream;
      try {
        s = await l.accept();
      } catch (e) {
        break;
      }
      ACCEPTED.add(1);
      spawn(answer(s, closing));
    }
  });
}
";

/// Sequential requests to one origin reuse one connection from the pool, at one worker and at
/// four (where a request's task may run on another worker than the one that parked it).
#[test]
fn sequential_fetches_reuse_one_pooled_connection() {
    let src = format!(
        "{SERVER}
async function main() {{
  const l = await listen(\"127.0.0.1:0\");
  serve(l, false);
  const base = `http://127.0.0.1:${{l.port}}`;
  const seen: string[] = [];
  for (let i = 0; i < 20; i++) {{
    const text = await (await fetch(`${{base}}/${{i}}`)).text();
    if (!seen.includes(text)) {{
      seen.push(text);
    }}
  }}
  // One after another from other tasks: each takes the connection the last one parked.
  for (let i = 0; i < 5; i++) {{
    const text = await spawn(async () => await (await fetch(`${{base}}/t${{i}}`)).text());
    if (!seen.includes(text)) {{
      seen.push(text);
    }}
  }}
  const last = await (await fetch(`${{base}}/last`)).text();
  if (!seen.includes(last)) {{
    seen.push(last);
  }}
  console.log(seen.join(\",\"));
  console.log(`accepted ${{ACCEPTED.load()}}`);
  l.close();
}}
"
    );
    same_everywhere("fetch_pool_reuse", &src, "conn 1\naccepted 1\n");
}

/// A server that closes each connection after its answer: the pool finds a parked connection
/// closed when it looks at it, so the next POST (which is never retried) gets a new one instead
/// of failing; a refused connect fails with Node's wording.
#[test]
fn a_closed_pooled_connection_is_not_reused_and_a_refused_connect_fails() {
    let src = format!(
        "{SERVER}
import {{ IoError }} from \"velt/io\";

async function main() {{
  const l = await listen(\"127.0.0.1:0\");
  serve(l, true);
  const base = `http://127.0.0.1:${{l.port}}`;
  for (let i = 0; i < 3; i++) {{
    // A POST is never sent again on another connection: one on a closed connection fails.
    console.log(await (await fetch(`${{base}}/${{i}}`, {{ method: \"POST\" }})).text());
    // The server has closed the connection before the pool looks at it (on loopback its FIN
    // is in this side's queue once its close returns).
    while (CLOSED.load() < i + 1) {{
      await yieldNow();
    }}
  }}
  l.close();
  const gone = await listen(\"127.0.0.1:0\");
  const port = gone.port;
  gone.close();
  try {{
    await fetch(`http://127.0.0.1:${{port}}/`);
    console.log(\"connected\");
  }} catch (e) {{
    const io = e as IoError;
    console.log(`${{io.code}} ${{io.message.startsWith(\"fetch failed: \")}}`);
  }}
}}
"
    );
    same_everywhere(
        "fetch_pool_closed",
        &src,
        "conn 1\nconn 2\nconn 3\nECONNREFUSED true\n",
    );
}
