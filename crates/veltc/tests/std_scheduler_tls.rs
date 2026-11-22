//! TLS listeners on the reactor of the executor in Velt (std/net/tls/fd_ops.velt, #2649): a
//! native program serves `listenTls` with a P-256 key with the TLS 1.3 server in Velt over the
//! reactor's sockets (no runtime handle, no task in between). Each program runs at one worker and
//! at four (killed after a minute: a lost wake hangs) and must print the same, and its binary
//! holds the Velt server.

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

/// `name.velt` built: the binary's path.
fn build(dir: &Path, name: &str) -> std::path::PathBuf {
    let exe = dir.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(dir.join(format!("{name}.velt")))
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

/// `exe` run with `threads` workers: its stdout.
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

/// A message only the TLS server in Velt has (std/net/tls/fd_ops.velt), as data in every build.
const SERVER_MARKER: &[u8] = b"the handshake timed out";

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Builds `src` and runs it at one worker and at four: every run prints `want`, and the binary
/// has the TLS server in Velt.
fn same_everywhere(name: &str, src: &str, want: &str) {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    std::fs::write(dir.path().join("_tls.velt"), tls_material()).unwrap();
    std::fs::write(dir.path().join(format!("{name}.velt")), src).unwrap();
    let exe = build(dir.path(), name);
    assert_eq!(run(&exe, 1), want, "{name}: one worker");
    assert_eq!(run(&exe, 4), want, "{name}: four workers");
    // A string the server embeds as data: function names are in the .pdb on Windows, not the .exe.
    assert!(
        contains(&std::fs::read(&exe).unwrap(), SERVER_MARKER),
        "{name}: no TLS server in Velt"
    );
}

/// An echo server written as http-server reads and writes (`tryRead` into its own buffer,
/// `readable` when nothing is there, `writeBytesLater`, `drained`), against four `connectTls` clients
/// at once that each read in one task while another writes, a Velt client (`startTls` over
/// `connect`), and a client that doesn't speak TLS. ALPN picks the server's first protocol the
/// client offers. A client that waits for each echo finds the server reading a little at a time,
/// its `readable` answered from what the stream holds; that server greets first, before the
/// handshake is done.
#[test]
fn a_tls_echo_server_serves_clients_on_the_reactor() {
    same_everywhere(
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

async function connectTlsClient(i: number, port: number): Promise<string> {
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
    clients.push(spawn(connectTlsClient(i, l.port)));
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

/// What a TLS server reports for a connection that ends badly, as rustls's listener did: a
/// plaintext request fails at its first bytes with decode_error (not after the handshake's time
/// limit), a client that leaves during the handshake or after it without close_notify is an EOF,
/// and a client's alert during the handshake (the client refusing the server's certificate) is read,
/// though unprotected, and named as the client's.
#[test]
fn a_tls_server_reports_what_ended_a_connection() {
    same_everywhere(
        "tls_ends",
        r#"import { connect, connectTls, listenTls, startTls, TcpStream } from "velt/net";
import { IoError } from "velt/io";
import { TEST_CERT, TEST_KEY } from "./_tls";

async function serve(conn: TcpStream): Promise<string> {
  try {
    await conn.read();
    await conn.read();
    conn.close();
    return "read";
  } catch (e) {
    conn.close();
    return e instanceof IoError ? `${e.code} ${e.message}` : `${e}`;
  }
}

async function main() {
  const l = await listenTls("127.0.0.1:0", { cert: TEST_CERT, key: TEST_KEY });
  const server = spawn((async (): Promise<string[]> => {
    const out: string[] = [];
    for (let i = 0; i < 4; i++) {
      out.push(await serve(await l.accept()));
    }
    return out;
  })());
  // Not TLS: the server answers decode_error (alert 50) and ends.
  const plain = await connect(`127.0.0.1:${l.port}`);
  await plain.write("GET / HTTP/1.1\r\n\r\n");
  const answer = await plain.read();
  while ((await plain.read()).length > 0) {}
  plain.close();
  console.log(answer.join(","));
  // Gone during the handshake.
  const early = await connect(`127.0.0.1:${l.port}`);
  early.close();
  // A client that doesn't trust the test certificate alerts during the handshake.
  try {
    await connectTls(`localhost:${l.port}`, {});
  } catch (e) {
    console.log("client refused");
  }
  // Gone after the handshake without close_notify.
  const tcp = await connect(`127.0.0.1:${l.port}`);
  const s = await startTls(tcp, { serverName: "localhost", verify: false });
  await s.write("x");
  s.close();
  for (const line of await server) {
    console.log(line);
  }
  l.close();
}
"#,
        "21,3,3,0,2,2,50\nclient refused\n\
         EILSEQ TLS: not a TLS record (no TLS version in its header)\n\
         EOF TLS: the connection closed during the handshake\n\
         EILSEQ TLS: the client sent the alert unknown_ca (48)\n\
         EOF TLS: the connection closed without close_notify\n",
    );
}

/// Readiness of a TLS stream is what a `tryRead` finds: nothing right after the handshake (the
/// socket was read during it), something once the peer wrote, nothing after it was read, whether
/// the wait only looks, has a limit or a signal.
#[test]
fn a_tls_stream_is_readable_when_a_read_finds_bytes() {
    same_everywhere(
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

/// A program that can't listen or connect with TLS doesn't load the TLS server in Velt, nor the
/// TLS operations its streams would check for (std/net/tls/fd_hook.velt): a plain TCP echo builds
/// neither, and still runs.
#[test]
fn a_program_without_tls_options_builds_no_tls_server() {
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
    let exe = build(dir.path(), "plain");
    assert_eq!(run(&exe, 1), "hi\n");
    let bin = std::fs::read(&exe).unwrap();
    for marker in [
        SERVER_MARKER,
        b"tryServerHandshake",
        b"fd__ops",
        b"fd__server",
    ] {
        assert!(
            !contains(&bin, marker),
            "{}: built without TLS options",
            String::from_utf8_lossy(marker)
        );
    }
}

/// What only the TLS client or server in Velt holds: their modules' symbols.
const TLS_MARKERS: &[&[u8]] = &[
    b"fd__client",
    b"client__handshake",
    b"fd__server",
    b"server__handshake",
];

/// Panics if the binary `exe` holds the TLS client or server (`what` says which program it is).
fn assert_no_tls(exe: &Path, what: &str) {
    let bin = std::fs::read(exe).unwrap();
    for marker in TLS_MARKERS {
        assert!(
            !contains(&bin, marker),
            "{}: {what} has the TLS code (#2392: loaded by reachability)",
            String::from_utf8_lossy(marker)
        );
    }
}

/// A `fetch` of an `http:` literal (or a template literal that starts with one) that cannot follow
/// a redirect (`redirect: "manual"` or `"error"`) takes `fetchPlain` (`@variant`,
/// std/net/http/fetch/fetch.velt), which has no TLS client: the program loads and builds none of
/// the TLS code, and still fetches. Without the option it can be redirected to `https:`, as in
/// Node, so the same program keeps the client.
#[test]
fn an_http_only_fetch_without_redirects_builds_no_tls_client() {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let src = r#"import { listen } from "velt/net";

async function main() {
  const l = await listen("127.0.0.1:0");
  const s = spawn(async () => {
    const c = await l.accept();
    await c.read();
    await c.write("HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
    c.close();
  });
  const res = await fetch(`http://127.0.0.1:${l.port}/`, { redirect: "manual" });
  console.log(res.status, await res.text());
  await s;
  l.close();
}
"#;
    std::fs::write(dir.path().join("fetch_http.velt"), src).unwrap();
    let exe = build(dir.path(), "fetch_http");
    assert_eq!(run(&exe, 1), "200 ok\n");
    assert_no_tls(&exe, "an http:-only fetch with redirect: \"manual\"");
    // Following redirects (the default) keeps the client: the markers are what it builds.
    let follows = src.replace(", { redirect: \"manual\" }", "");
    std::fs::write(dir.path().join("fetch_follow.velt"), follows).unwrap();
    let exe = build(dir.path(), "fetch_follow");
    assert_eq!(run(&exe, 1), "200 ok\n");
    let bin = std::fs::read(&exe).unwrap();
    assert!(
        contains(&bin, b"fd__client"),
        "an http: fetch that follows redirects must keep the TLS client"
    );
}

/// `serve` with options written without `tls` takes `servePlain` (`@variant`,
/// packages/http-server/src/standard.velt), which never calls `listenTls`: a plain server builds
/// none of the TLS code, and still serves.
#[test]
fn a_plain_serve_builds_no_tls_server() {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let server = manifest().join("../../packages/http-server");
    let at = server.to_string_lossy().replace('\\', "/");
    std::fs::write(
        dir.path().join("package.velt"),
        format!(
            "import type {{ Package }} from \"velt/package\";\n\nexport const pkg: Package = {{\n  name: \"plain-serve\",\n  version: \"0.0.0\",\n  dependencies: {{ \"http-server\": {{ path: \"{at}\" }} }},\n}};\n"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/main.velt"),
        r#"import { serve } from "http-server";

async function main() {
  const server = await serve({ port: 0 }, (req: Request) => new Response("hi"));
  const res = await fetch(`http://127.0.0.1:${server.port}/`, { redirect: "manual" });
  console.log(await res.text());
  server.close();
}
"#,
    )
    .unwrap();
    let exe = dir.path().join(format!(
        "plain-serve{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(run(&exe, 1), "hi\n");
    assert_no_tls(&exe, "a plain serve");
}

/// A WebSocket echo over a plain connection: the server end is a `WebSocket` made over the
/// accepted stream (`webSocketOver`), the client end comes from `connectWebSocket(URL)`.
fn ws_program(url: &str) -> String {
    format!(
        r#"import {{ listen }} from "velt/net";
import {{ connectWebSocket, webSocketAccept, webSocketOver }} from "velt/net/websocket";
import {{ utf8Decode }} from "velt/encoding";

async function main() {{
  const l = await listen("127.0.0.1:0");
  const s = spawn(async () => {{
    const c = await l.accept();
    const head = utf8Decode(await c.read());
    let key = "";
    for (const line of head.split("\r\n")) {{
      if (line.toLowerCase().startsWith("sec-websocket-key:")) {{
        key = line.slice(18).trim();
      }}
    }}
    await c.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${{webSocketAccept(key)}}\r\n\r\n`);
    const ws = webSocketOver(c, true);
    const m = await ws.receive();
    await ws.send(`echo ${{m == null ? "" : m.text}}`);
    await ws.close();
  }});
  if (l.port > 0) {{
    const ws = await connectWebSocket({url});
    await ws.send("hi");
    const m = await ws.receive();
    console.log(m == null ? "none" : m.text);
    await ws.close();
  }}
  await s;
  l.close();
}}
"#
    )
}

/// `connectWebSocket` of a `ws:` literal takes `connectWebSocketPlain` (`@variant`,
/// std/net/websocket/connect.velt), which never calls `connectTls`, and the `WebSocket` of the
/// server end needs none: a `ws:`-only program builds none of the TLS code, and still talks. A
/// `wss:` literal in the same program keeps the client (the markers are what it builds).
#[test]
fn a_ws_only_websocket_program_builds_no_tls_client() {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let plain = ws_program("`ws://127.0.0.1:${l.port}/`");
    std::fs::write(dir.path().join("ws_plain.velt"), &plain).unwrap();
    let exe = build(dir.path(), "ws_plain");
    assert_eq!(run(&exe, 1), "echo hi\n");
    assert_no_tls(&exe, "a ws:-only connectWebSocket and WebSocket");
    // `wss:` is never dialled here (the port check), but the client is built for it.
    let secure = plain.replace(
        "    await ws.close();\n  }\n  await s;",
        "    await ws.close();\n  }\n  if (l.port == 0) {\n    await connectWebSocket(\"wss://localhost/\");\n  }\n  await s;",
    );
    assert_ne!(secure, plain);
    std::fs::write(dir.path().join("ws_secure.velt"), &secure).unwrap();
    let exe = build(dir.path(), "ws_secure");
    assert_eq!(run(&exe, 1), "echo hi\n");
    let bin = std::fs::read(&exe).unwrap();
    assert!(
        contains(&bin, b"fd__client"),
        "a wss: connectWebSocket must build the TLS client"
    );
}
