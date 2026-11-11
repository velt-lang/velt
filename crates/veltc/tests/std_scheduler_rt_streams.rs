//! Deferred writes on the runtime's streams under the executor in Velt (#2817): a program
//! compiled with `VELT_SCHEDULER=std` keeps what `writeLater` queues on a runtime-owned stream
//! (`streamPair`, `connectTls`) in Velt and hands it to the runtime's `write` once the poll ends
//! (std/net/rt_later.velt): the runtime sends its own deferred bytes only after polls of its own
//! tasks, which never run on the Velt worker. And the end of such a program gives back the send
//! buffers of the reactor's sockets dropped without `close()` (sched_io_table.velt
//! `releaseBuffers`), which a leak check counts. Each program is built with and without the
//! variable, both binaries run with `VELT_THREADS=1` (killed after a minute: lost bytes hang)
//! and must print the same.

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

/// `src` built (with `VELT_SCHEDULER=std` when `std`) and run with the block counters on: its
/// output, and its binary.
fn build_and_run(dir: &Path, name: &str, src: &str, std: bool) -> (Output, Vec<u8>) {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
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
        .env("VELT_RC_STATS", "1")
        .env_remove("VELT_SCHEDULER")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A watchdog, not a synchronization: bytes never sent hang the program.
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
    (o, std::fs::read(&exe).unwrap())
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The `blocks=A/F` of a run's `rc stats` line (a debug runtime's), if it has one.
fn blocks(o: &Output) -> Option<(u64, u64)> {
    let err = String::from_utf8_lossy(&o.stderr);
    let stats = err.lines().find(|l| l.starts_with("rc stats:"))?;
    let b = stats
        .split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))?;
    let (a, f) = b.split_once('/')?;
    Some((a.parse().ok()?, f.parse().ok()?))
}

/// Builds and runs `src` both ways: each prints `want` and frees every block it allocated; the
/// std build holds `marker`'s code (std's file of the path under test), the other doesn't.
fn same_on_both(name: &str, src: &str, want: &str, marker: &[u8]) {
    if !cfg!(unix) {
        // Windows has no reactor in Velt yet: its sockets stay the runtime's in both builds.
        return;
    }
    let dir = test_dir::TestDir::new();
    // The goldens' test certificate and key (`TEST_CA`, `TEST_CERT`, `TEST_KEY`).
    std::fs::write(
        dir.path().join("_tls.velt"),
        include_str!("../../../tests/golden/std/_tls.velt"),
    )
    .unwrap();
    for std in [false, true] {
        let (o, bin) = build_and_run(dir.path(), name, src, std);
        let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
        assert_eq!(out, want, "{name} (std: {std})");
        assert_eq!(
            contains(&bin, marker),
            std,
            "{name} (std: {std}): its code in Velt"
        );
        // Only a debug runtime counts blocks.
        if let Some((a, f)) = blocks(&o) {
            assert_eq!(a, f, "{name} (std: {std}): blocks allocated / freed");
        }
    }
}

/// The issue's case and its neighbours on a `streamPair`: deferred bytes reach the peer once
/// the poll ends, with `drained` and without (`readable`), a `write` and a `shutdown` go behind
/// them, `tryWrite` takes nothing while they wait, `close` sends them first, and 4 MiB go
/// through with `drained` as backpressure.
#[test]
fn deferred_writes_on_a_runtime_stream_reach_the_peer() {
    same_on_both(
        "pair_later",
        "import { streamPair, TcpStream } from \"velt/net\";

async function readAll(s: TcpStream): Promise<string> {
  let out = \"\";
  while (true) {
    const b = await s.readString();
    if (b == \"\") {
      return out;
    }
    out = out + b;
  }
}

async function main() {
  const [x, y] = streamPair();
  x.writeBytesLater([1, 2, 3, 4, 5]);
  await yieldNow();
  await x.drained(0);
  console.log(\"read\", (await y.read()).length);
  x.close();
  y.close();

  const [a, b] = streamPair();
  a.writeLater(\"one \");
  a.writeLater(\"two \");
  await a.write(\"three \");
  a.writeLater(\"four\");
  a.shutdown();
  console.log(await readAll(b));
  a.close();
  b.close();

  const [c, d] = streamPair();
  c.writeLater(\"ping\");
  await d.readable();
  console.log(await d.readString());
  c.writeLater(\"x\");
  console.log(c.tryWrite([1, 2, 3]));
  await c.drained();
  console.log(await d.readString());
  c.writeLater(\"bye\");
  c.close();
  console.log(await readAll(d));
  d.close();
  try {
    c.writeLater(\"late\");
    await c.drained();
    console.log(\"no error\");
  } catch (err) {
    console.log(err.code);
  }

  const [e, f] = streamPair();
  const reader = spawn((async () => {
    let n = 0;
    let sum = 0;
    while (true) {
      const got = await f.read();
      if (got.length == 0) {
        return [n, sum];
      }
      n = n + got.length;
      for (const v of got) {
        sum = (sum + v) % 1000003;
      }
    }
  })());
  const chunk: u8[] = [];
  for (let i = 0; i < 65536; i++) {
    chunk.push((i % 251) as u8);
  }
  for (let i = 0; i < 64; i++) {
    e.writeBytesLater(chunk);
    await e.drained(262144);
  }
  e.shutdown();
  console.log(await reader);
  e.close();
  f.close();
}
",
        "read 5\none two three four\nping\n0\nx\nbye\nEBADF\n[ 4194304, 105628 ]\n",
        b"rt_later",
    );
}

/// The same on TLS streams, which stay the runtime's under std: a `connectTls` client and the
/// runtime listener's accepted stream both answer with `writeLater` only, and the client ends
/// with `writeLater`, `shutdown` and `close` before the server has read anything.
#[test]
fn deferred_writes_on_runtime_tls_streams_reach_the_peer() {
    same_on_both(
        "tls_later",
        "import { connectTls, listenTls } from \"velt/net\";
import { TEST_CA, TEST_CERT, TEST_KEY } from \"./_tls\";

async function main() {
  const server = await listenTls(\"127.0.0.1:0\", { cert: TEST_CERT, key: TEST_KEY });
  const serving = spawn((async () => {
    const conn = await server.accept();
    let total = 0;
    while (true) {
      const chunk = await conn.read();
      if (chunk.length == 0) {
        break;
      }
      total += chunk.length;
      conn.writeBytesLater(chunk);
    }
    conn.writeLater(`|${total}`);
    conn.shutdown();
    conn.close();
  })());
  const c = await connectTls(`localhost:${server.port}`, { ca: TEST_CA });
  c.writeLater(\"hello\");
  let got = \"\";
  while (got.length < 5) {
    got += await c.readString();
  }
  console.log(got);
  c.writeLater(\"wor\");
  c.writeLater(\"ld\");
  await c.drained(0);
  got = \"\";
  while (got.length < 5) {
    got += await c.readString();
  }
  console.log(got);
  c.writeLater(\"!\");
  c.shutdown();
  let rest = \"\";
  while (true) {
    const t = await c.readString();
    if (t == \"\") {
      break;
    }
    rest += t;
  }
  console.log(rest);
  c.close();
  await serving;
  server.close();
}
",
        "hello\nworld\n!|11\n",
        b"rt_later",
    );
}

/// A stream given bytes with `writeLater` and dropped without `close()` stays open to the end,
/// as the runtime's do; its send buffer is given back then, so a leak check finds nothing (the
/// accepted socket here, and an unclosed `streamPair` end).
#[test]
fn an_unclosed_stream_s_send_buffer_is_freed_at_the_end() {
    same_on_both(
        "unclosed",
        "import { connect, listen, streamPair } from \"velt/net\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  const server = spawn((async () => {
    const conn = await l.accept();
    const first = await conn.readString();
    conn.writeLater(\"x\");
    await conn.drained();
    return first;
  })());
  const c = await connect(`127.0.0.1:${l.port}`);
  await c.write(\"hello\");
  console.log(await server, await c.readString());
  c.close();
  l.close();

  const [x, y] = streamPair();
  x.writeLater(\"kept\");
  console.log(await y.readString());
}
",
        "hello x\nkept\n",
        b"listenOn",
    );
}
