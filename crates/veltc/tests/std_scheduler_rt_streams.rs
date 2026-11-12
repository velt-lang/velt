//! Deferred writes on the runtime's streams under the executor in Velt (#2817): a program
//! compiled with `VELT_SCHEDULER=std` keeps what `writeLater` queues on a runtime-owned stream
//! (`streamPair`, `connectTls`) in Velt and hands it to the runtime's `write` once the poll ends
//! (std/net/rt_later.velt): the runtime sends its own deferred bytes only after polls of its own
//! tasks, which never run on the Velt worker. And the end of such a program gives back the send
//! buffers of the reactor's sockets dropped without `close()` (sched_io_table.velt
//! `releaseBuffers`), which a leak check counts. Each program is built with and without the
//! variable, both binaries run with `VELT_THREADS=1` and `4` (the flusher is a task other workers
//! may take; killed after a minute: lost bytes hang) and must print the same.

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use no_window::command;

mod no_window;
mod runtime_support;
mod std_unix;
mod test_dir;

/// `src` built (with `VELT_SCHEDULER=std` when `std`): the binary.
fn build(dir: &Path, name: &str, src: &str, std: bool) -> std::path::PathBuf {
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
    exe
}

/// `exe` run with `threads` workers and the block counters on: its output.
fn run(exe: &Path, name: &str, std: bool, threads: &str) -> Output {
    let mut child = command(exe)
        .env("VELT_THREADS", threads)
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
            panic!("{name} (std: {std}, {threads} workers): still running after a minute");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    assert!(
        o.status.success(),
        "{name} (std: {std}, {threads} workers): {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    o
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

/// Which runs of [`same_on_both_counted`] must free every block they allocated.
#[derive(Clone, Copy, PartialEq)]
enum Counted {
    /// The runs with 1 worker. With more, a task can end on another worker as the program ends
    /// and free its block after the counts are taken, the end-of-program race #2847 fixes.
    OneWorker,
    /// None.
    Never,
}

/// Builds and runs `src` both ways: each prints `want`, and frees every block it allocated when
/// run with 1 worker ([`Counted::OneWorker`]); the std build holds `marker`'s code (std's file of
/// the path under test), the other doesn't.
fn same_on_both(name: &str, src: &str, want: &str, marker: &[u8]) {
    same_on_both_counted(name, src, want, marker, Counted::OneWorker);
}

/// [`same_on_both`], with the block counts compared only in the runs `counted` names.
fn same_on_both_counted(name: &str, src: &str, want: &str, marker: &[u8], counted: Counted) {
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
        let exe = build(dir.path(), name, src, std);
        let bin = std::fs::read(&exe).unwrap();
        assert_eq!(
            contains(&bin, marker),
            std,
            "{name} (std: {std}): its code in Velt"
        );
        for threads in ["1", "4"] {
            let o = run(&exe, name, std, threads);
            let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
            assert_eq!(out, want, "{name} (std: {std}, {threads} workers)");
            let compared = match counted {
                Counted::OneWorker => threads == "1",
                Counted::Never => false,
            };
            // Only a debug runtime counts blocks.
            if let Some((a, f)) = blocks(&o).filter(|_| compared) {
                assert_eq!(
                    a, f,
                    "{name} (std: {std}, {threads} workers): blocks allocated / freed"
                );
            }
        }
    }
}

/// The shadows' white-box tests (std_unix/net/rt_later_shadow.test.velt): every flag of a
/// stream's flags word is a bit of its own, and a flusher given up at the end finds nothing left
/// to send. They import the scheduler's modules, which only link on macOS and Linux.
#[cfg(unix)]
#[test]
fn rt_later_shadow_white_box_tests_pass() {
    runtime_support::build_native_runtime(Path::new(env!("CARGO_MANIFEST_DIR")));
    std_unix::velt_test_in_std_copy(
        "net/rt_later_shadow.test.velt",
        include_str!("std_unix/net/rt_later_shadow.test.velt"),
    );
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
/// with `writeLater`, `shutdown` and `close` before the server has read anything. Block counts
/// are not compared: a reactor thread of the runtime that still holds a waker of a finished task
/// when the counts are taken gives its reference back after them, one block (#2842).
#[test]
fn deferred_writes_on_runtime_tls_streams_reach_the_peer() {
    same_on_both_counted(
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
        Counted::Never,
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

/// `close()` with deferred bytes still to send closes the stream at once, as the runtime's does:
/// later operations report `EBADF` (none of their bytes reaches the peer), a read waiting on the
/// closed stream ends, and the deferred bytes are all sent all the same.
#[test]
fn close_with_deferred_bytes_closes_at_once_and_still_sends_them() {
    same_on_both(
        "close_now",
        "import { streamPair } from \"velt/net\";

async function main() {
  const [x, y] = streamPair();
  const peer = spawn((async () => {
    let n = 0;
    while (true) {
      const got = await y.read();
      if (got.length == 0) {
        return n;
      }
      n += got.length;
    }
  })());
  const big: u8[] = Buffer.alloc(8 * 1024 * 1024);
  x.writeBytesLater(big);
  x.close();
  try {
    x.writeLater(\"a\");
    console.log(\"writeLater ok\");
  } catch (e) {
    console.log(\"writeLater\", e.code);
  }
  console.log(\"tryWrite\", x.tryWrite([1, 2]));
  try {
    await x.write(\"bc\");
    console.log(\"write ok\");
  } catch (e) {
    console.log(\"write\", e.code);
  }
  try {
    await x.drained();
    console.log(\"drained ok\");
  } catch (e) {
    console.log(\"drained\", e.code);
  }
  console.log(\"peer got\", await peer);
  y.close();
}
",
        "writeLater EBADF\ntryWrite 0\nwrite EBADF\ndrained EBADF\npeer got 8388608\n",
        b"rt_later",
    );
    // Block counts not compared: a runtime thread still holding a waker of the finished reader
    // (its read waited on the closed stream) gives the reference back after the counts, at any
    // number of workers, one block (#2842).
    same_on_both_counted(
        "close_ends_read",
        "import { streamPair } from \"velt/net\";

async function main() {
  const [x, y] = streamPair();
  const reader = spawn((async () => {
    try {
      const got = await x.read();
      return `read ${got.length}`;
    } catch (e) {
      return `read ${e.code}`;
    }
  })());
  await yieldNow();
  const big: u8[] = Buffer.alloc(8 * 1024 * 1024);
  x.writeBytesLater(big);
  x.close();
  console.log(await reader);
  let n = 0;
  while (true) {
    const got = await y.read();
    if (got.length == 0) {
      break;
    }
    n += got.length;
  }
  console.log(\"peer got\", n);
  y.close();
}
",
        "read EBADF\npeer got 8388608\n",
        b"rt_later",
        Counted::Never,
    );
}

/// `writeLater` on a closed runtime stream reports `EBADF` every time, whether it deferred bytes
/// before its close or not.
#[test]
fn write_later_on_a_closed_runtime_stream_reports_ebadf() {
    same_on_both(
        "later_after_close",
        "import { streamPair } from \"velt/net\";

async function main() {
  const [x, y] = streamPair();
  x.close();
  for (let i = 0; i < 4; i++) {
    try {
      x.writeLater(\"late\");
      console.log(\"no throw\");
    } catch (e) {
      console.log(e.code);
    }
    await yieldNow();
  }
  const [a, b] = streamPair();
  a.writeLater(\"first\");
  console.log(await b.readString());
  a.close();
  for (let i = 0; i < 2; i++) {
    try {
      a.writeLater(\"late\");
      console.log(\"no throw\");
    } catch (e) {
      console.log(e.code);
    }
    await yieldNow();
  }
  y.close();
  b.close();
}
",
        "EBADF\nEBADF\nEBADF\nEBADF\nfirst\nEBADF\nEBADF\n",
        b"rt_later",
    );
}

/// Bytes deferred to a peer that never reads do not keep the program alive once `main` returns,
/// as the runtime's background sending doesn't. (The flusher given up is a task left over, whose
/// blocks no executor frees at that end: not compared.)
#[test]
fn deferred_bytes_do_not_keep_the_program_alive_after_main() {
    same_on_both_counted(
        "later_exit",
        "import { streamPair } from \"velt/net\";

async function main() {
  const [x, y] = streamPair();
  const big: u8[] = Buffer.alloc(8 * 1024 * 1024);
  x.writeBytesLater(big);
  console.log(\"returning\");
}
",
        "returning\n",
        b"rt_later",
        Counted::Never,
    );
}

/// A failed deferred write reports the runtime's status as it is (code and message), to
/// `drained` and to the next `writeLater`.
#[test]
fn a_failed_deferred_write_reports_the_runtimes_status() {
    same_on_both(
        "later_error",
        "import { streamPair } from \"velt/net\";

async function main() {
  const [x, y] = streamPair();
  y.close();
  await yieldNow();
  x.writeLater(\"hello\");
  try {
    await x.drained();
    console.log(\"drained ok\");
  } catch (e) {
    console.log(\"drained\", e.code, e.message);
  }
  try {
    x.writeLater(\"again\");
    console.log(\"writeLater ok\");
  } catch (e) {
    console.log(\"writeLater\", e.code, e.message);
  }
  x.close();
}
",
        "drained EPIPE broken pipe\nwriteLater EPIPE broken pipe\n",
        b"rt_later",
    );
}

/// A program that ends with `process.exit` while a runtime stream with a deferred-write buffer
/// is still open ends as the default build does. Its block counts are not compared: `process.exit`
/// ends in the runtime with no hook of std's on the way, and other workers may still be using the
/// buffers (sched_io_table.velt `releaseBuffers`).
#[test]
fn process_exit_with_an_open_runtime_stream_ends_the_program() {
    same_on_both_counted(
        "later_process_exit",
        "import { streamPair } from \"velt/net\";

async function main() {
  const [x, y] = streamPair();
  x.writeLater(\"kept\");
  console.log(await y.readString());
  process.exit(0);
}
",
        "kept\n",
        b"rt_later",
        Counted::Never,
    );
}
