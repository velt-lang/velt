//! velt/net on the reactor of the executor in Velt (std/lang/hooks/sched_io.velt,
//! std/net/tcp_sched.velt, #2649 step 2): a program compiled with `VELT_SCHEDULER=std` opens
//! non-blocking sockets the worker waits on in kqueue or epoll, sends what `writeLater` queued at
//! the end of each poll and before it waits, and is woken from the runtime's threads through the
//! reactor. Each program is built with and without the variable, both binaries run with
//! `VELT_THREADS=1` (killed after a minute: a lost wake hangs) and must print the same; the one
//! built with it holds the sockets in Velt, the other doesn't.

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

/// `src` built (with `VELT_SCHEDULER=std` when `std`) and run: its stdout, and its binary.
fn build_and_run(dir: &Path, name: &str, src: &str, std: bool) -> (String, Vec<u8>) {
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
    let mut child = command(&exe)
        .env("VELT_THREADS", "1")
        .env_remove("VELT_SCHEDULER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A watchdog, not a synchronization: a lost wake would hang the test run.
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(60) {
            child.kill().unwrap();
            panic!("{name} (std: {std}): still running after a minute");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    assert!(
        o.status.success(),
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

/// Builds and runs `src` both ways: each prints `want`; only the std build has sockets in Velt.
fn same_on_both(name: &str, src: &str, want: &str) {
    if !cfg!(unix) {
        // Windows has no reactor in Velt yet: its sockets stay the runtime's in both builds.
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (rt, rt_bin) = build_and_run(dir.path(), name, src, false);
    let (std, std_bin) = build_and_run(dir.path(), name, src, true);
    assert_eq!(rt, want, "{name}: the runtime's sockets");
    assert_eq!(std, want, "{name}: sockets on the reactor in Velt");
    assert!(
        contains(&std_bin, b"listenOn"),
        "{name}: no sockets in Velt"
    );
    assert!(
        !contains(&rt_bin, b"listenOn"),
        "{name}: sockets in Velt without the variable"
    );
}

/// The server answers only with `writeBytesLater` and then waits for the next read: what it
/// queued must be sent before the worker waits, or the client never sees it.
#[test]
fn an_echo_server_sends_what_it_queued_before_the_worker_waits() {
    same_on_both(
        "tcp_echo",
        "import { connect, listen } from \"velt/net\";
import { utf8Encode } from \"velt/encoding\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  const server = spawn((async () => {
    const s = await l.accept();
    while (true) {
      const b = await s.read();
      if (b.length === 0) {
        break;
      }
      s.writeBytesLater(b);
      s.writeLater(\"|\");
    }
    s.close();
  })());
  const c = await connect(`127.0.0.1:${l.port}`);
  console.log(c.peerAddr() === `127.0.0.1:${l.port}`);
  await c.write(\"hello\");
  console.log(await c.readString());
  await c.writeBytes(utf8Encode(\"world\"));
  console.log(await c.readString());
  c.shutdown();
  console.log(await c.readString() === \"\");
  c.close();
  await server;
  l.close();
}
",
        "true\nhello|\nworld|\ntrue\n",
    );
}

/// Readiness: nothing to read yet (`tryRead` 0, `readableWithin` runs out), then the peer
/// writes after a sleep on the runtime's timer thread, whose wake reaches the worker waiting in
/// the reactor, and the socket's event wakes the reader.
#[test]
fn a_reader_waits_for_readiness_and_a_deadline_ends_a_wait() {
    same_on_both(
        "tcp_ready",
        "import { connect, listen } from \"velt/net\";
import { utf8Encode } from \"velt/encoding\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  const server = spawn((async () => {
    const s = await l.accept();
    await s.read(1);
    await sleep(30);
    await s.writeBytes(utf8Encode(\"ab\"));
    await s.read(1);
    s.close();
  })());
  const c = await connect(`127.0.0.1:${l.port}`);
  const buf: u8[] = [0, 0, 0, 0];
  console.log(c.tryRead(buf));
  console.log(await c.readableWithin(20));
  await c.write(\"go\");
  await c.readable();
  let n = c.tryRead(buf) ?? -1;
  while (n === 1) {
    n = n + (c.tryRead(buf, 1) ?? 0);
  }
  console.log(n, buf[0], buf[1]);
  console.log(await c.readableWithin(10000));
  console.log(c.tryRead(buf) === null);
  c.close();
  await server;
  l.close();
}
",
        "0\nfalse\n2 97 98\ntrue\ntrue\n",
    );
}

/// More than the socket takes at once: what it can't take stays queued and goes out on its
/// writable events; `drained` waits for that, and the reader gets every byte in order.
#[test]
fn queued_bytes_beyond_the_socket_go_out_on_writable_events() {
    same_on_both(
        "tcp_drain",
        "import { connect, listen } from \"velt/net\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  const block: u8[] = [];
  for (let i = 0; i < 65536; i++) {
    block.push((i % 251) as u8);
  }
  const server = spawn((async () => {
    const s = await l.accept();
    for (let i = 0; i < 64; i++) {
      s.writeBytesLater(block);
      await s.drained(1 << 20);
    }
    await s.drained();
    s.close();
  })());
  const c = await connect(`127.0.0.1:${l.port}`);
  let total = 0;
  let ok = true;
  while (true) {
    const b = await c.read();
    if (b.length === 0) {
      break;
    }
    for (const x of b) {
      ok = ok && x === ((total % 65536) % 251);
      total++;
    }
  }
  console.log(total, ok);
  c.close();
  await server;
  l.close();
}
",
        "4194304 true\n",
    );
}

/// Errors read as the runtime's: a refused connect is ECONNREFUSED; a closed handle is EBADF.
#[test]
fn a_refused_connect_and_a_closed_handle_report_the_runtimes_codes() {
    same_on_both(
        "tcp_errors",
        "import { connect, listen } from \"velt/net\";
import { IoError } from \"velt/io\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  const port = l.port;
  l.close();
  try {
    await connect(`127.0.0.1:${port}`);
    console.log(\"connected\");
  } catch (e) {
    console.log((e as IoError).code);
  }
  const l2 = await listen(\"127.0.0.1:0\");
  const c = await connect(`127.0.0.1:${l2.port}`);
  const s = await l2.accept();
  c.close();
  try {
    await c.write(\"x\");
  } catch (e) {
    console.log((e as IoError).code);
  }
  console.log((await s.read()).length);
  s.close();
  l2.close();
}
",
        "ECONNREFUSED\nEBADF\n0\n",
    );
}
