//! velt/net on the reactor of the executor in Velt (std/lang/hooks/sched_io.velt,
//! std/net/tcp_sched.velt, #2649 step 2): a program compiled with `VELT_SCHEDULER=std` opens
//! non-blocking sockets the worker waits on in kqueue or epoll, sends what `writeLater` queued at
//! the end of each poll and before it waits, and is woken from the runtime's threads through the
//! reactor. Each program is built with the variable, runs with `VELT_THREADS=1` (killed after a
//! minute: a lost wake hangs) and must print what Node prints, with the sockets in Velt.

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

/// `src` built with `VELT_SCHEDULER=std` and run: its stdout, and its binary.
fn build_and_run(dir: &Path, name: &str, src: &str) -> (String, Vec<u8>) {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!(
        "{name}_std{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
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
            panic!("{name}: still running after a minute");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    assert!(
        o.status.success(),
        "{name}: {}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    (out, std::fs::read(&exe).unwrap())
}

/// What marks std function `function` in a built program: its name, or on Windows, where a PE
/// file names no functions and a debug build's PDB only the exported ones, the panic locations
/// its file's code carries (`std/net/fd_socket.velt:line:col`).
fn marker(function: &str) -> String {
    if cfg!(windows) {
        "std/net/fd_socket.velt:".to_string()
    } else {
        function.to_string()
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Builds and runs `src`: it prints `want`, with sockets in Velt.
fn prints(name: &str, src: &str, want: &str) {
    let dir = test_dir::TestDir::new();
    prints_in(dir.path(), name, src, want);
}

/// [`prints`] in folder `dir` (which may hold packages the program uses).
fn prints_in(dir: &Path, name: &str, src: &str, want: &str) {
    runtime_support::build_native_runtime(manifest());
    let (std, std_bin) = build_and_run(dir, name, src);
    assert_eq!(std, want, "{name}");
    assert!(
        contains(&std_bin, marker("listenOn").as_bytes()),
        "{name}: no sockets in Velt"
    );
}

/// The server answers only with `writeBytesLater` and then waits for the next read: what it
/// queued must be sent before the worker waits, or the client never sees it.
#[test]
fn an_echo_server_sends_what_it_queued_before_the_worker_waits() {
    prints(
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
    let src = "import { connect, listen } from \"velt/net\";
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
  // Readable means a tryRead may find something: the readiness of \"ab\" holds until a read
  // finds nothing, so the first wake can come before the server's end arrives (0, wait again).
  let woke = await c.readableWithin(10000);
  let last = c.tryRead(buf);
  while (woke && last === 0) {
    woke = await c.readableWithin(10000);
    last = c.tryRead(buf);
  }
  console.log(woke);
  console.log(last === null);
  c.close();
  await server;
  l.close();
}
";
    let want = "0\nfalse\n2 97 98\ntrue\ntrue\n";
    if !cfg!(windows) {
        prints("tcp_ready", src, want);
        return;
    }
    // On Windows the read after the server's close sometimes reports an error (a reset) instead
    // of the end: the last line is either; the waits before it are compared.
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let (out, _) = build_and_run(dir.path(), "tcp_ready", src);
    let (waits, last) = out.trim_end().rsplit_once('\n').unwrap_or_default();
    assert_eq!(waits, "0\nfalse\n2 97 98\ntrue", "tcp_ready");
    assert!(last == "true" || last == "false", "tcp_ready: {out}");
}

/// More than the socket takes at once: what it can't take stays queued and goes out on its
/// writable events; `drained` waits for that, and the reader gets every byte in order.
#[test]
fn queued_bytes_beyond_the_socket_go_out_on_writable_events() {
    prints(
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
    prints(
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

/// The `.velt` files under `dir` (recursively) that use velt/net or the http-server package.
fn net_users(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            net_users(&path, out);
        } else if path.extension().is_some_and(|e| e == "velt") {
            let src = std::fs::read_to_string(&path).unwrap();
            if ["\"velt/net\"", "\"velt/net/", "\"http-server\""]
                .iter()
                .any(|n| src.contains(n))
            {
                out.push(path);
            }
        }
    }
}

/// `velt check` of `file` (from its folder, so a golden finds its package) with
/// `VELT_SCHEDULER=std`, or `velt build` into `out` when given (lowering finds what checking
/// doesn't, such as two signatures of one C symbol): whether it passed, and its diagnostics.
fn check(file: &Path, out: Option<&Path>) -> (bool, String) {
    let mut c = command(env!("CARGO_BIN_EXE_velt"));
    match out {
        Some(o) => c
            .arg("build")
            .arg(file.file_name().unwrap())
            .arg("-o")
            .arg(o),
        None => c.arg("check").arg(file.file_name().unwrap()),
    };
    c.current_dir(file.parent().unwrap())
        .env("VELT_SCHEDULER", "std");
    let o = c.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (o.status.success(), text)
}

/// The `std-scheduler` variants of velt/net (std/net/tcp_sched.velt, the reactors) keep the
/// default's signatures, error types included: every velt/net user among the goldens (the
/// http-server package's included) compiles under `VELT_SCHEDULER=std` as its golden says: it
/// fails exactly when the golden has an `.err` file. A variant whose function threw `TypeError` failed
/// `serve`'s `throws` clause; a C declaration of errno's location with another signature than
/// the compiler's clashed in every program reading `lastOsError()`.
#[test]
fn velt_net_users_compile_as_their_goldens_say() {
    if !cfg!(unix) {
        // Some of these goldens use POSIX system functions Windows doesn't have.
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let golden = manifest().join("../../tests/golden");
    let mut files = vec![];
    net_users(&golden, &mut files);
    files.sort();
    // Programs reading `lastOsError()`, built: the compiler declares errno's location then.
    let dir = test_dir::TestDir::new();
    for name in [
        "system_function_errno_await",
        "system_function_subarray_owner",
    ] {
        let f = golden.join(format!("lang/{name}.velt"));
        let exe = |tag: &str| {
            dir.path()
                .join(format!("{name}{tag}{}", std::env::consts::EXE_SUFFIX))
        };
        let std = check(&f, Some(&exe("_std")));
        assert!(std.0, "{name}:\n{}", std.1);
    }
    assert!(files.len() > 50, "{} velt/net users found", files.len());
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let differ = std::sync::Mutex::new(vec![]);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(f) = files.get(i) else { break };
                let std = check(f, None);
                if std.0 == f.with_extension("err").exists() {
                    differ.lock().unwrap().push(format!(
                        "{} ({}): {}",
                        f.display(),
                        if std.0 { "compiles" } else { "fails" },
                        std.1.trim()
                    ));
                }
            });
        }
    });
    let differ = differ.into_inner().unwrap();
    assert!(differ.is_empty(), "{}", differ.join("\n"));
}

/// A pending accept is pending I/O: it keeps the process alive after `main` until the listener
/// is closed, as in Node. The client runs from an `unref()`'d timer, which runs only while
/// something else keeps the process alive: without the accept counted, the program ends at
/// once with nothing printed after `main`.
#[test]
fn a_listening_server_keeps_the_process_alive_until_closed() {
    prints(
        "listener_alive",
        "import { connect, listen } from \"velt/net\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  const port = l.port;
  spawn((async () => {
    const s = await l.accept();
    console.log(`accepted ${await s.readString()}`);
    s.close();
    l.close();
  })());
  setTimeout(async () => {
    const c = await connect(`127.0.0.1:${port}`);
    console.log(\"connected\");
    await c.write(\"hi\");
    c.close();
  }, 50).unref();
  console.log(\"main done\");
}
",
        "main done\nconnected\naccepted hi\n",
    );
}

/// A program's own declarations of C symbols the reactor and velt/net's sockets call (`read`,
/// `write` and `fcntl` as system functions; `accept4` or `accept`, `malloc`, `free` and
/// `getsockname` in a package of its own): one C symbol has one signature in a program, so the
/// std variants declare them as a program does, pointers as `Ptr` and `size_t` as `usize`. With
/// addresses as `usize` the std build failed to compile ("C function `write` has two
/// signatures"), as `lang/system_function_subarray_owner` did on Linux.
#[test]
fn a_program_declaring_the_reactors_c_functions_compiles() {
    if !cfg!(unix) {
        // The declarations below are POSIX's.
        return;
    }
    let accept = if cfg!(target_os = "linux") {
        "declare c(\"c\") function accept4(fd: i32, addr: Ptr<u8> | null, len: Ptr<u32> | null, flags: i32): i32;

export function acceptBad(): i32 {
  // SAFETY: no address; a descriptor that is none.
  return unsafe { accept4(-1, null, null, 0) };
}
"
    } else {
        "declare c(\"c\") function accept(fd: i32, addr: Ptr<u8> | null, len: Ptr<u32> | null): i32;

export function acceptBad(): i32 {
  // SAFETY: no address; a descriptor that is none.
  return unsafe { accept(-1, null, null) };
}
"
    };
    let lib = format!(
        "import {{ Ptr }} from \"velt/ffi\";

declare c(\"c\") function malloc(n: usize): Ptr<u8> | null;
declare c(\"c\") function free(p: Ptr<u8> | null): void;
declare c(\"c\") function getsockname(fd: i32, addr: Ptr<u8> | null, len: Ptr<u32> | null): i32;
{accept}
export function nameBad(): i32 {{
  // SAFETY: no address; a descriptor that is none.
  return unsafe {{ getsockname(-1, null, null) }};
}}

export function allocates(): bool {{
  // SAFETY: malloc has no precondition.
  const p = unsafe {{ malloc(16) }};
  // SAFETY: free takes what malloc returned.
  unsafe {{
    free(p);
  }}
  return p !== null;
}}
"
    );
    let dir = test_dir::TestDir::new();
    let pkg = dir.path().join("cdecl");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(
        pkg.join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = { name: \"cdecl\", version: \"0.1.0\", native: { c: { c: { unchecked: true } } } };\n",
    )
    .unwrap();
    std::fs::write(pkg.join("src/lib.velt"), lib).unwrap();
    std::fs::write(
        dir.path().join("package.velt"),
        "import type { Package } from \"velt/package\";\n\nexport const pkg: Package = {\n  name: \"app\",\n  version: \"0.1.0\",\n  allowNative: [\"cdecl\"],\n  dependencies: { cdecl: { path: \"./cdecl\" } },\n};\n",
    )
    .unwrap();
    prints_in(
        dir.path(),
        "own_c_declarations",
        "import { connect, listen } from \"velt/net\";
import { acceptBad, allocates, nameBad } from \"cdecl\";

declare system function read(fd: i32, buf: u8[]): isize;
declare system function write(fd: i32, buf: readonly u8[]): isize;
declare system function fcntl(fd: i32, cmd: i32, arg: i32): i32;

async function main() {
  const buf: u8[] = Buffer.alloc(4);
  console.log(read(-1, buf), write(-1, buf), fcntl(-1, 1, 0), acceptBad(), nameBad());
  console.log(allocates());
  const l = await listen(\"127.0.0.1:0\");
  const server = spawn((async () => {
    const s = await l.accept();
    s.writeLater(await s.readString());
    s.close();
  })());
  const c = await connect(`127.0.0.1:${l.port}`);
  await c.write(\"echo\");
  console.log(await c.readString());
  c.close();
  await server;
  l.close();
}
",
        "-1 -1 -1 -1 -1\ntrue\necho\n",
    );
}

/// `readableWithin(-1, signal)` has no time limit, as the runtime's: only readiness or the signal
/// ends it. The reactor's version took a limit of 0 or less as "look now", so a wait with only a
/// signal said "not readable" at once; http-server waits so for the next request on a kept-alive
/// connection and closed it as idle, and the client's next request on it failed ("other side
/// closed", `std/http_server/http_close_releases_handler`). The client writes after a delay and
/// a `tryRead` finding nothing comes first, so each wait is pending when it starts.
#[test]
fn a_read_wait_with_no_limit_waits_for_the_bytes() {
    let want = "0\nsignal only: true\none\n0\nno limit: true\ntwo\naborted: false\n";
    let dir = test_dir::TestDir::new();
    prints_in(
        dir.path(),
        "readable_no_limit",
        "import { connect, listen } from \"velt/net\";
import { AbortController } from \"velt/task\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  const stop = new AbortController();
  // The client writes only after the server has looked (nothing there yet) and is about to wait:
  // the server resolves these just before each wait, and the wait runs up to its park before the
  // client's task gets the worker back.
  const first = Promise.withResolvers<void>();
  const second = Promise.withResolvers<void>();
  const server = spawn((async () => {
    const s = await l.accept();
    const buf: u8[] = Buffer.alloc(16);
    // Nothing has come yet: the socket is not readable now, so each wait below waits.
    console.log(s.tryRead(buf));
    first.resolve();
    console.log(\"signal only:\", await s.readableWithin(-1, stop.signal));
    console.log(await s.readString());
    console.log(s.tryRead(buf));
    second.resolve();
    console.log(\"no limit:\", await s.readableWithin(-1));
    console.log(await s.readString());
    stop.abort();
    console.log(\"aborted:\", await s.readableWithin(-1, stop.signal));
    s.close();
  })());
  const c = await connect(`127.0.0.1:${l.port}`);
  await first.promise;
  await c.write(\"one\");
  await second.promise;
  await c.write(\"two\");
  await server;
  c.close();
  l.close();
}
",
        want,
    );
}

/// A read wait with only a signal ends when the signal aborts while it waits: by `abort()` from
/// another task, and by an `AbortSignal.timeout` (http-server's idle deadline); with input it ends
/// readable. Under the executor in Velt the signal's wait is polled inside the socket's wait
/// (std/lang/hooks/sched_io_wait.velt `waitAlso`), not raced against it.
#[test]
fn a_read_wait_ends_when_its_signal_aborts_while_it_waits() {
    prints(
        "readable_until_abort",
        "import { connect, listen } from \"velt/net\";
import { AbortController, AbortSignal } from \"velt/task\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  const c = await connect(`127.0.0.1:${l.port}`);
  const s = await l.accept();
  // Nothing has come yet: a read finds nothing, so the waits below wait.
  console.log(s.tryRead(Buffer.alloc(16)));
  const stop = new AbortController();
  const waiting = Promise.withResolvers<void>();
  const aborter = spawn((async () => {
    await waiting.promise;
    stop.abort();
  })());
  waiting.resolve();
  console.log(\"abort:\", await s.readableWithin(-1, stop.signal));
  await aborter;
  console.log(\"timeout:\", await s.readableWithin(-1, AbortSignal.timeout(20)));
  const later = new AbortController();
  const wrote = spawn((async () => {
    await c.write(\"x\");
  })());
  console.log(\"input:\", await s.readableWithin(-1, later.signal));
  await wrote;
  console.log(await s.readString());
  s.close();
  c.close();
  l.close();
}
",
        "0\nabort: false\ntimeout: false\ninput: true\nx\n",
    );
}

/// Requests one after another on one kept-alive connection to an http-server served under the
/// executor in Velt: between them the server waits for the next with only its idle timeout's
/// signal, and must not close the connection meanwhile (fetch reuses it from its pool at once, so
/// a close crossed the next request and failed it).
#[test]
fn an_http_server_keeps_a_connection_alive_between_requests() {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let pkg = manifest().join("../../packages/http-server");
    let pkg = pkg.canonicalize().unwrap();
    std::fs::write(
        dir.path().join("package.velt"),
        format!(
            "import type {{ Package }} from \"velt/package\";

export const pkg: Package = {{
  name: \"keep-alive-test\",
  version: \"0.0.0\",
  dependencies: {{ \"http-server\": {{ path: {:?} }} }},
}};
",
            pkg.to_string_lossy()
        ),
    )
    .unwrap();
    let src = "import { serve } from \"http-server\";

async function main() {
  const server = await serve({ port: 0 }, async (req: Request): Promise<Response> => {
    return new Response(`got ${new URL(req.url).pathname}`);
  });
  const base = `http://127.0.0.1:${server.port}`;
  for (let i = 0; i < 10; i++) {
    console.log(await (await fetch(`${base}/${i}`)).text());
  }
  await server.shutdown();
}
";
    let want: String = (0..10).map(|i| format!("got /{i}\n")).collect();
    let (std, std_bin) = build_and_run(dir.path(), "keep_alive", src);
    assert_eq!(std, want);
    assert!(
        contains(&std_bin, marker("listenOn").as_bytes()),
        "no sockets in Velt"
    );
}

/// A connect waited on by a worker while worker 0 parks too: the program goes on until the
/// connects are done. A parked worker that took its reactor's events counted its wait out and
/// queued the woken task while its park word was still up, so worker 0's end-of-program scan
/// could see neither and end the program with nothing printed (half the runs of 100 connects on
/// a Mac; now the worker counts itself awake before it delivers). The std build with 4 workers,
/// 10 runs.
#[test]
fn the_program_runs_on_while_another_worker_waits_for_a_connect() {
    if !cfg!(any(target_os = "macos", target_os = "linux", windows)) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let src = r#"import { connect, listen, TcpListener } from "velt/net";
import { channel, Channel } from "velt/sync";

async function clients(port: i64): Promise<i64> {
  for (let i: i64 = 0; i < 100; i++) {
    const c = await connect(`127.0.0.1:${port}`);
    c.close();
  }
  return 100;
}

async function serve(l: TcpListener, go: Channel<i64>): Promise<void> {
  const n = (await go.receive()) ?? 0;
  for (let i: i64 = 0; i < n; i++) {
    (await l.accept()).close();
  }
  l.close();
}

async function main() {
  const l = await listen("127.0.0.1:0");
  const go = channel<i64>(1);
  const server = spawn(serve(l, go), { worker: 0 });
  const n = await spawn(clients(l.port as i64), { worker: 1 });
  await go.send(n);
  await server;
  console.log(`connected ${n} times`);
}
"#;
    let file = dir.path().join("connects.velt");
    std::fs::write(&file, src).unwrap();
    let exe = dir.path().join(if cfg!(windows) {
        "connects.exe"
    } else {
        "connects"
    });
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    for run in 0..10 {
        let o = command(&exe)
            .env("VELT_THREADS", "4")
            .env_remove("VELT_SCHEDULER")
            .output()
            .unwrap();
        assert_eq!(
            (
                o.status.code(),
                String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
            ),
            (Some(0), "connected 100 times\n".to_string()),
            "run {run}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

/// `src` built with `VELT_SCHEDULER=std` and run `times` times with each of `threads` workers,
/// the block counters on (their wait at the end included): each run ends with status 0 and
/// prints `want`. For the ends of programs that race another thread: a task finishing on another
/// worker, a reactor's late wake. The block counts are not compared: a runtime thread that holds
/// a waker of a finished task and has not started giving it back when the counts are taken
/// frees one block after them (405/404 in about 1 of 30 runs on Linux; #2842).
fn std_runs_ending(name: &str, src: &str, want: &str, threads: &[&str], times: usize) {
    if !cfg!(unix) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    // The goldens' test certificate and key (`TEST_CA`, `TEST_CERT`, `TEST_KEY`).
    std::fs::write(
        dir.path().join("_tls.velt"),
        include_str!("../../../tests/golden/std/_tls.velt"),
    )
    .unwrap();
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
    for threads in threads {
        for i in 0..times {
            let o = command(&exe)
                .env("VELT_THREADS", threads)
                .env("VELT_RC_STATS", "1")
                .env_remove("VELT_SCHEDULER")
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
            assert_eq!(
                (o.status.code(), out.as_str()),
                (Some(0), want),
                "{name} ({threads} workers, run {i}): {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
    }
}

/// A runtime write still in flight when its stream is closed goes on, and the program ends with
/// the peer's view whole: the task that wrote may finish on another worker as `main` ends. Its
/// block counts are not compared (#2842; [`std_runs_ending`]); the deterministic
/// `a_task_ending_last_on_another_worker_frees_its_block_first` of std_scheduler_bounds covers
/// the `finish` order.
#[test]
fn a_write_in_flight_across_close_ends_the_program() {
    std_runs_ending(
        "close_inflight",
        "import { streamPair } from \"velt/net\";

async function main() {
  const [x, y] = streamPair();
  spawn((async () => {
    const big: u8[] = Buffer.alloc(8 * 1024 * 1024);
    try {
      await x.writeBytes(big);
    } catch (e) {
      // Closed before the write began.
    }
  })());
  await yieldNow();
  x.close();
  let n = 0;
  while (true) {
    const got = await y.read();
    if (got.length == 0) {
      break;
    }
    n += got.length;
  }
  console.log(\"peer got all or nothing:\", n == 0 || n == 8 * 1024 * 1024);
  y.close();
}
",
        "peer got all or nothing: true\n",
        &["4"],
        30,
    );
}

/// TLS streams closed at the end of `main` end the program, with a reactor thread of the runtime
/// that may wake a finished task late (its socket's readiness after the close) while the block
/// counters wait for wakers giving their reference back (task/waker.rs `giving_back`). Its block
/// counts are not compared: a waker not yet giving back is not waited for (#2842;
/// [`std_runs_ending`]).
#[test]
fn tls_streams_closed_at_the_end_end_the_program() {
    std_runs_ending(
        "tls_end",
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
      await conn.writeBytes(chunk);
    }
    await conn.write(`|${total}`);
    conn.shutdown();
    conn.close();
  })());
  const c = await connectTls(`localhost:${server.port}`, { ca: TEST_CA });
  await c.write(\"hello\");
  let got = \"\";
  while (got.length < 5) {
    got += await c.readString();
  }
  console.log(got);
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
        "hello\n|5\n",
        &["1", "4"],
        20,
    );
}

/// A name that does not resolve: on Windows `getaddrinfo`'s failure is a Winsock error, which
/// Rust words as an OS error ("No such host is known. (os error 11001)"), elsewhere an `EAI_*`
/// code ("failed to lookup address information: …"), as the runtime words it, for a lookup and
/// a connect; the lookup's error is `ENOTFOUND` with the name.
#[test]
fn a_failed_name_lookup_reads_as_with_the_runtime() {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let src = "import { connect } from \"velt/net\";
import { lookup } from \"velt/net/dns\";

async function main() {
  try {
    await lookup(\"doesnotexist.invalid\");
    console.log(\"found\");
  } catch (e) {
    console.log(`lookup: ${e} ${e.code}`);
  }
  try {
    await connect(\"doesnotexist.invalid:80\");
    console.log(\"connected\");
  } catch (e) {
    console.log(`connect: ${e} ${e.code}`);
  }
}
";
    let (std, _) = build_and_run(dir.path(), "lookup_fails", src);
    let lines: Vec<&str> = std.lines().collect();
    assert_eq!(lines.len(), 2, "{std}");
    assert!(
        lines[0].starts_with("lookup: ") && lines[0].contains("ENOTFOUND doesnotexist.invalid"),
        "{std}"
    );
    assert!(lines[1].starts_with("connect: "), "{std}");
    let os = if cfg!(windows) {
        "No such host is known"
    } else {
        "failed to lookup address information"
    };
    assert!(lines.iter().all(|l| l.contains(os)), "{std}");
}
