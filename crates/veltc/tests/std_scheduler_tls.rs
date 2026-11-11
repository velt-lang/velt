//! TLS listeners on the reactor of the executor in Velt (std/net/tls/fd_ops.velt, #2649): a
//! program compiled with `VELT_SCHEDULER=std` serves `listenTls` with the TLS 1.3 server in Velt
//! over the reactor's sockets (no runtime handle, no task in between), the runtime's otherwise.
//! Each program is built both ways; the runtime's build runs at one worker, the std build at one
//! and at four (killed after a minute: a lost wake hangs), and all must print the same. The std
//! binary holds the Velt server, the other doesn't.

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

/// The goldens' throwaway CA and localhost certificate (tests/golden/std/_tls.velt).
fn tls_material() -> String {
    std::fs::read_to_string(manifest().join("../../tests/golden/std/_tls.velt")).unwrap()
}

/// `name.velt` built (with `VELT_SCHEDULER=std` when `std`): the binary's path.
fn build(dir: &Path, name: &str, std: bool) -> std::path::PathBuf {
    let exe = dir.join(format!(
        "{name}{}{}",
        if std { "_std" } else { "" },
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let mut b = command(env!("CARGO_BIN_EXE_velt"));
    b.arg("build")
        .arg(dir.join(format!("{name}.velt")))
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env_remove("VELT_SCHEDULER");
    if std {
        b.env("VELT_SCHEDULER", "std");
    }
    let o = b.output().unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    exe
}

/// `exe` run with `threads` workers: its stdout.
fn run(exe: &Path, threads: u32) -> String {
    let mut child = command(exe)
        .env("VELT_THREADS", threads.to_string())
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
            panic!(
                "{} (threads {threads}): still running after a minute",
                exe.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    assert!(
        o.status.success(),
        "{} (threads {threads}): {}{}",
        exe.display(),
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Builds `src` both ways and runs it: every run prints `want`, and only the std build has the
/// TLS server in Velt.
fn same_on_both(name: &str, src: &str, want: &str) {
    if !cfg!(unix) {
        // Windows has no reactor in Velt here: its sockets stay the runtime's in both builds.
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("_tls.velt"), tls_material()).unwrap();
    std::fs::write(dir.path().join(format!("{name}.velt")), src).unwrap();
    let rt = build(dir.path(), name, false);
    let std = build(dir.path(), name, true);
    assert_eq!(run(&rt, 1), want, "{name}: the runtime's TLS");
    assert_eq!(run(&std, 1), want, "{name}: TLS in Velt, one worker");
    assert_eq!(run(&std, 4), want, "{name}: TLS in Velt, four workers");
    let marker = b"tryServerHandshake";
    assert!(
        contains(&std::fs::read(&std).unwrap(), marker),
        "{name}: no TLS server in Velt"
    );
    assert!(
        !contains(&std::fs::read(&rt).unwrap(), marker),
        "{name}: the TLS server in Velt without the variable"
    );
}

/// An echo server written as http-server reads and writes (`tryRead` into its own buffer,
/// `readable` when nothing is there, `writeBytesLater`, `drained`), against four rustls clients
/// at once that each read in one task while another writes, a Velt client (`startTls` over
/// `connect`), and a client that doesn't speak TLS. ALPN picks the server's first protocol the
/// client offers. A client that waits for each echo finds the server reading a little at a time,
/// its `readable` answered from what the stream holds; that server greets first, before the
/// handshake is done.
#[test]
fn a_tls_echo_server_serves_clients_on_the_reactor() {
    same_on_both(
        "tls_echo",
        r#"import { connect, connectTls, listenTls, startTls, TcpStream } from "velt/net";
import { TEST_CA, TEST_CERT, TEST_KEY } from "./_tls";

async function echo(conn: TcpStream, greet: bool): Promise<string> {
  const buf: u8[] = Buffer.alloc(7);
  let total = 0;
  try {
    if (greet) {
      // Before the handshake: held until the keys are there.
      conn.writeLater("hello ");
    }
    while (true) {
      // Wait first, read a little: most of a record stays buffered in the stream for the next
      // `readable`, which must not wait on the socket for it.
      await conn.readable();
      const n = conn.tryRead(buf);
      if (n === null) {
        break;
      }
      if (n === 0) {
        continue;
      }
      total += n;
      conn.writeBytesLater(buf, 0, n);
      await conn.drained(65536);
    }
    conn.writeLater(`|${total}`);
    await conn.drained();
    conn.shutdown();
  } catch (e) {
    conn.close();
    return "refused";
  }
  conn.close();
  return `${total}`;
}

async function readAll(conn: TcpStream): Promise<string> {
  let out = "";
  while (true) {
    const text = await conn.readString();
    if (text == "") {
      return out;
    }
    out += text;
  }
}

async function rustlsClient(i: number, port: number): Promise<string> {
  const c = await connectTls(`localhost:${port}`, { ca: TEST_CA, alpn: ["http/1.1"] });
  const reader = spawn(readAll(c));
  let sent = 0;
  for (let k = 0; k < 40; k++) {
    const chunk = `${i}.${k};`.repeat(300);
    await c.write(chunk);
    sent += chunk.length;
  }
  c.shutdown();
  const got = await reader;
  const alpn = await c.alpnProtocol();
  c.close();
  return `${alpn} ${got.length == sent + `|${sent}`.length} ${got.endsWith(`|${sent}`)}`;
}

// Each message waits for its echo before the next goes.
async function pingPong(port: number): Promise<string> {
  const c = await connectTls(`localhost:${port}`, { ca: TEST_CA });
  let hello = "";
  while (hello.length < 6) {
    hello += await c.readString();
  }
  let ok = hello == "hello ";
  let sent = 0;
  for (let k = 0; k < 20; k++) {
    const m = `ping ${k} `.repeat(3);
    await c.write(m);
    sent += m.length;
    let got = "";
    while (got.length < m.length) {
      got += await c.readString();
    }
    ok = ok && got == m;
  }
  c.shutdown();
  const tail = await readAll(c);
  c.close();
  return `${ok} ${tail == `|${sent}`}`;
}

async function veltClient(port: number): Promise<string> {
  const tcp = await connect(`127.0.0.1:${port}`);
  const s = await startTls(tcp, { serverName: "localhost", verify: false, alpn: ["h2"] });
  const data: u8[] = Buffer.alloc(50000);
  for (let i = 0; i < data.length; i++) {
    data[i] = (i % 251) as u8;
  }
  await s.writeBytes(data, 0);
  await s.shutdown();
  const got: u8[] = [];
  while (true) {
    const b = await s.read();
    if (b.length == 0) {
      break;
    }
    for (const x of b) {
      got.push(x);
    }
  }
  let same = got.length >= data.length;
  for (let i = 0; same && i < data.length; i++) {
    same = got[i] == data[i];
  }
  const alpn = await s.alpnProtocol();
  s.close();
  return `${alpn} ${same} ${got.length - data.length}`;
}

async function main() {
  const l = await listenTls("127.0.0.1:0", {
    cert: TEST_CERT,
    key: TEST_KEY,
    alpn: ["h2", "http/1.1"],
  });
  const server = spawn(async () => {
    const served: Promise<string>[] = [];
    for (let i = 0; i < 7; i++) {
      const conn = await l.accept();
      served.push(spawn(echo(conn, i == 4)));
    }
    const totals: string[] = [];
    for (const p of served) {
      totals.push(await p);
    }
    totals.sort();
    return totals.join(" ");
  });
  const clients: Promise<string>[] = [];
  for (let i = 0; i < 4; i++) {
    clients.push(spawn(rustlsClient(i, l.port)));
  }
  for (const c of clients) {
    console.log(await c);
  }
  console.log(await pingPong(l.port));
  console.log(await veltClient(l.port));
  const plain = await connect(`127.0.0.1:${l.port}`);
  await plain.write("GET / HTTP/1.1\r\n\r\n");
  while ((await plain.read()).length > 0) {}
  plain.close();
  console.log(await server);
  l.close();
}
"#,
        "http/1.1 true true\nhttp/1.1 true true\nhttp/1.1 true true\nhttp/1.1 true true\n\
         true true\nh2 true 6\n\
         450 50000 57000 57000 57000 57000 refused\n",
    );
}

/// Readiness of a TLS stream is what a `tryRead` finds: nothing right after the handshake (the
/// socket was read during it), something once the peer wrote, nothing after it was read, whether
/// the wait only looks, has a limit or a signal.
#[test]
fn a_tls_stream_is_readable_when_a_read_finds_bytes() {
    same_on_both(
        "tls_readable",
        r#"import { connectTls, listenTls } from "velt/net";
import { TEST_CA, TEST_CERT, TEST_KEY } from "./_tls";

async function main() {
  const l = await listenTls("127.0.0.1:0", { cert: TEST_CERT, key: TEST_KEY });
  const server = spawn(async () => {
    const c = await l.accept();
    await c.alpnProtocol();
    const buf: u8[] = Buffer.alloc(100);
    console.log("after the handshake:", await c.readableWithin(0), c.tryRead(buf));
    await c.write("go");
    console.log("signal:", await c.readableWithin(-1, AbortSignal.timeout(30000)));
    console.log("read:", c.tryRead(buf));
    console.log("after the read:", await c.readableWithin(0), await c.readableWithin(1));
    await c.write("again");
    console.log("limit:", await c.readableWithin(30000));
    console.log("read:", c.tryRead(buf));
    c.close();
  });
  const k = await connectTls(`localhost:${l.port}`, { ca: TEST_CA });
  console.log(await k.readString());
  await k.write("hello");
  console.log(await k.readString());
  await k.write("world!");
  await server;
  k.close();
  l.close();
}
"#,
        "after the handshake: false 0\ngo\nsignal: true\nread: 5\nafter the read: false false\n\
         again\nlimit: true\nread: 6\n",
    );
}

/// A program that names no TLS option (`cert`) doesn't load the TLS server in Velt, nor the TLS
/// operations its streams would check for (std/prelude/global/tls_reactor.velt): a plain TCP
/// echo under `VELT_SCHEDULER=std` builds neither, and still runs.
#[test]
fn a_program_without_tls_options_builds_no_tls_server() {
    if !cfg!(unix) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    std::fs::write(
        dir.path().join("plain.velt"),
        r#"import { connect, listen } from "velt/net";

async function main() {
  const l = await listen("127.0.0.1:0");
  const s = spawn(async () => {
    const c = await l.accept();
    await c.writeBytes(await c.read());
    c.close();
  });
  const c = await connect(`127.0.0.1:${l.port}`);
  await c.write("hi");
  console.log(await c.readString());
  await s;
  c.close();
  l.close();
}
"#,
    )
    .unwrap();
    let exe = build(dir.path(), "plain", true);
    assert_eq!(run(&exe, 1), "hi\n");
    let bin = std::fs::read(&exe).unwrap();
    for marker in [&b"tryServerHandshake"[..], b"fd__ops", b"fd__server"] {
        assert!(
            !contains(&bin, marker),
            "{}: built without TLS options",
            String::from_utf8_lossy(marker)
        );
    }
}
