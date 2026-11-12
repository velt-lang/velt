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

/// Builds and runs `src` both ways: each prints `want`; only the std build has sockets in Velt.
fn same_on_both(name: &str, src: &str, want: &str) {
    let dir = test_dir::TestDir::new();
    same_on_both_in(dir.path(), name, src, want, want);
}

/// [`same_on_both`] in folder `dir` (which may hold packages the program uses), where the
/// runtime's executor prints `rt_want`.
fn same_on_both_in(dir: &Path, name: &str, src: &str, want: &str, rt_want: &str) {
    runtime_support::build_native_runtime(manifest());
    let (rt, rt_bin) = build_and_run(dir, name, src, false);
    let (std, std_bin) = build_and_run(dir, name, src, true);
    assert_eq!(rt, rt_want, "{name}: the runtime's sockets");
    assert_eq!(std, want, "{name}: sockets on the reactor in Velt");
    assert!(
        contains(&std_bin, marker("listenOn").as_bytes()),
        "{name}: no sockets in Velt"
    );
    assert!(
        !contains(&rt_bin, marker("listenOn").as_bytes()),
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
  console.log(await c.readableWithin(10000));
  console.log(c.tryRead(buf) === null);
  c.close();
  await server;
  l.close();
}
";
    let want = "0\nfalse\n2 97 98\ntrue\ntrue\n";
    if !cfg!(windows) {
        same_on_both("tcp_ready", src, want);
        return;
    }
    // On Windows the read after the server's close sometimes reports an error (a reset) instead
    // of the end, on both executors (CI runs saw either from the runtime's): the last line is
    // either; the waits before it are compared.
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for std in [false, true] {
        let (out, _) = build_and_run(dir.path(), "tcp_ready", src, std);
        let (waits, last) = out.trim_end().rsplit_once('\n').unwrap_or_default();
        assert_eq!(waits, "0\nfalse\n2 97 98\ntrue", "tcp_ready, std: {std}");
        assert!(
            last == "true" || last == "false",
            "tcp_ready, std: {std}: {out}"
        );
    }
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

/// `velt check` of `file` (from its folder, so a golden finds its package), with
/// `VELT_SCHEDULER=std` when `std`, or `velt build` into `out` when given (lowering finds what
/// checking doesn't, such as two signatures of one C symbol): whether it passed, and its
/// diagnostics.
fn check(file: &Path, std: bool, out: Option<&Path>) -> (bool, String) {
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
        .env_remove("VELT_SCHEDULER");
    if std {
        c.env("VELT_SCHEDULER", "std");
    }
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
/// http-server package's included, and the ones that must fail to compile) compiles the same
/// with and without `VELT_SCHEDULER=std`. A variant whose function threw `TypeError` failed
/// `serve`'s `throws` clause; a C declaration of errno's location with another signature than
/// the compiler's clashed in every program reading `lastOsError()`.
#[test]
fn velt_net_users_compile_the_same_on_both_executors() {
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
        let rt = check(&f, false, Some(&exe("")));
        let std = check(&f, true, Some(&exe("_std")));
        assert_eq!((rt.0, std.0), (true, true), "{name}:\n{}\n{}", rt.1, std.1);
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
                let (rt, std) = (check(f, false, None), check(f, true, None));
                if rt != std {
                    differ.lock().unwrap().push(format!(
                        "{}:\n  default ({}): {}\n  std ({}): {}",
                        f.display(),
                        rt.0,
                        rt.1.trim(),
                        std.0,
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
    same_on_both(
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
fn a_program_declaring_the_reactors_c_functions_compiles_on_both_executors() {
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
    same_on_both_in(
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
    // On Windows the runtime's executor (tokio's) reports the socket readable for the last wait,
    // where nothing has come; the reactor in Velt does not.
    let rt_want = if cfg!(windows) {
        "0\nsignal only: true\none\n0\nno limit: true\ntwo\naborted: true\n"
    } else {
        want
    };
    let dir = test_dir::TestDir::new();
    same_on_both_in(
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
        rt_want,
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
    let (rt, _) = build_and_run(dir.path(), "keep_alive", src, false);
    let (std, std_bin) = build_and_run(dir.path(), "keep_alive", src, true);
    assert_eq!(rt, want, "the runtime's sockets");
    assert_eq!(std, want, "sockets on the reactor in Velt");
    assert!(
        contains(&std_bin, marker("listenOn").as_bytes()),
        "no sockets in Velt"
    );
}

/// Windows' reactor (std/lang/hooks/reactor_afd.velt) and its Winsock calls (std/net/
/// calls_win32.velt), the threads and the parking: every C function a `VELT_SCHEDULER=std`
/// program using sockets, name lookups and files calls on Windows is one the link's import
/// libraries export (kernel32, ntdll, ws2_32, the synchronization library, the UCRT), debug and
/// release. Checked from any OS through the LLVM IR, since a Windows link needs Windows; the
/// POSIX functions the executor called (`clock_gettime`, `sched_yield`, `close`) exist in none.
#[test]
fn a_windows_program_calls_only_functions_its_import_libraries_export() {
    let kit = manifest().join("../velt_link/kit/windows");
    let mut exported = std::collections::HashSet::new();
    for lib in ["kernel32", "ntdll", "ws2_32", "synchronization", "ucrt"] {
        let def = std::fs::read_to_string(kit.join(format!("{lib}.def"))).unwrap();
        for line in def
            .lines()
            .skip_while(|l| !l.starts_with("EXPORTS"))
            .skip(1)
        {
            if let Some(name) = line.split_whitespace().next() {
                exported.insert(name.to_string());
            }
        }
    }
    let dir = test_dir::TestDir::new();
    std::fs::write(
        dir.path().join("main.velt"),
        "import { connect, listen } from \"velt/net\";
import { lookup } from \"velt/net/dns\";
import { readFile, writeFile } from \"velt/fs\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  const server = spawn((async () => {
    const s = await l.accept();
    s.writeBytesLater(await s.read());
    console.log(s.peerAddr());
    s.close();
  })());
  const c = await connect(`127.0.0.1:${l.port}`);
  await c.write(\"hi\");
  console.log(await c.readString(), await c.readableWithin(10));
  c.shutdown();
  c.close();
  await server;
  l.close();
  console.log((await lookup(\"localhost\")).length);
  await writeFile(\"x.txt\", \"y\");
  console.log(await readFile(\"x.txt\"));
}
",
    )
    .unwrap();
    for release in [false, true] {
        let mut c = command(env!("CARGO_BIN_EXE_velt"));
        c.args(["build", "main.velt", "--emit", "llvm"])
            .args(["--target", "x86_64-pc-windows-msvc"])
            .current_dir(dir.path())
            .env("VELT_SCHEDULER", "std");
        if release {
            c.arg("--release");
        }
        let o = c.output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let ir = String::from_utf8_lossy(&o.stdout);
        assert!(
            ir.contains("NtDeviceIoControlFile"),
            "no Windows reactor (release: {release})"
        );
        let missing: Vec<&str> = ir
            .lines()
            .filter(|l| l.starts_with("declare "))
            .filter_map(|l| l.split('@').nth(1)?.split('(').next())
            .map(|n| n.trim_matches('"'))
            .filter(|n| !n.starts_with("velt_") && !n.starts_with("llvm."))
            .filter(|n| !exported.contains(*n))
            .collect();
        assert!(missing.is_empty(), "release: {release}: {missing:?}");
    }
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
    let exe = dir.path().join(if cfg!(windows) { "connects.exe" } else { "connects" });
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

/// `src` (file `name`) built with `VELT_SCHEDULER=std` against std folder `std` (the
/// repository's when `None`): the binary.
fn build_std(dir: &Path, name: &str, src: &str, std: Option<&Path>) -> std::path::PathBuf {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
    let mut build = command(env!("CARGO_BIN_EXE_velt"));
    build
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir)
        .env("VELT_SCHEDULER", "std");
    if let Some(std) = std {
        build.env("VELT_STD", std);
    }
    let o = build.output().unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    exe
}

/// `exe` run with `threads` workers, counting its blocks (`VELT_RC_STATS`): its exit status
/// and output, stderr after stdout.
fn run_counted(exe: &Path, threads: &str) -> (bool, String) {
    let mut child = command(exe)
        .env("VELT_THREADS", threads)
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .env("VELT_RC_STATS", "1")
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
                "{} ({threads} workers): still running after a minute",
                exe.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let o = child.wait_with_output().unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
    .replace("\r\n", "\n");
    (o.status.success(), out)
}

/// Sockets opened and closed one after the other, each pair exchanging a byte: what every
/// socket's close leaves behind in the reactor shows up in the block counts or, in the
/// white-box copy below, as an event of a request that outlived its source.
const CHURN: &str = "import { connect, listen } from \"velt/net\";

async function main() {
  const l = await listen(\"127.0.0.1:0\");
  let n = 0;
  for (let i = 0; i < 40; i++) {
    const c = await connect(`127.0.0.1:${l.port}`);
    const s = await l.accept();
    await s.write(\"x\");
    n += (await c.read()).length;
    c.close();
    n += (await s.read()).length;
    s.close();
  }
  l.close();
  console.log(n);
}
";

/// A socket closed just before the program ends leaves its reactor state behind (on Windows a
/// poll request waiting for a completion nobody takes): none of it may count as a leaked block
/// (`blocks=A/F` with A = F), with one worker and several.
#[test]
fn a_socket_program_frees_every_block_it_counted() {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "churn", CHURN, None);
    for threads in ["1", "4"] {
        let (ok, out) = run_counted(&exe, threads);
        assert!(ok, "{threads} worker(s): {out}");
        assert!(out.starts_with("40\n"), "{threads} worker(s): {out}");
        let Some(stats) = out.lines().rev().find(|l| l.starts_with("rc stats:")) else {
            // Only the debug runtime counts (a `--release` test run links the release one).
            if cfg!(debug_assertions) {
                panic!("no rc stats in:\n{out}");
            }
            return;
        };
        let blocks = stats
            .split_whitespace()
            .find_map(|w| w.strip_prefix("blocks="))
            .and_then(|b| b.split_once('/'))
            .unwrap_or_else(|| panic!("no block counts in {stats}"));
        assert_eq!(blocks.0, blocks.1, "{threads} worker(s): {stats}");
    }
}

/// Windows' reactor (std/lang/hooks/reactor_afd_poll.velt): a poll completion queued on the
/// port when its socket closes comes later. Unless the socket left the reactor before its close
/// (`DEREGISTERS`, std/lang/hooks/sched_io_table.velt `closing`), it came with the source of a
/// socket that may be another by then, and submitted its poll again on a handle that may be
/// another socket's. White-box, in a copy of std whose requests remember their source's
/// generation and panic on a completion of a live request whose source moved on; a second copy
/// without the deregistration must panic, or the check checks nothing.
#[test]
fn a_closed_sockets_poll_completion_never_reaches_its_next_source() {
    if !cfg!(windows) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for deregisters in [true, false] {
        let std_copy = dir.path().join(format!("std_{deregisters}"));
        copy_tree(&manifest().join("../../std"), &std_copy);
        let read = |p: &str| {
            std::fs::read_to_string(std_copy.join(p))
                .unwrap()
                .replace("\r\n", "\n")
        };
        // The generation at a fixed offset: the copy's layout must be the one the patch knows.
        assert!(read("lang/hooks/sched_io.velt").contains("export const S_GEN: usize = 108;\n"));
        let poll = read("lang/hooks/reactor_afd_poll.velt");
        let patches = [
            (
                "const P_PENDING: usize = 84;\nconst P_SIZE: usize = 96;\n",
                "const P_PENDING: usize = 84;\nconst P_SIZE: usize = 96;\nconst P_GEN: usize = 88;\n",
            ),
            (
                "  m.st(p + P_SOURCE, source);\n",
                "  m.st(p + P_SOURCE, source);\n  m.st32(p + P_GEN, m.ld32(source + 108));\n",
            ),
            (
                "    m.lastingRelease(p);\n    return false;\n  }\n  const status",
                "    m.lastingRelease(p);\n    return false;\n  }\n  if (m.ld32(m.ld(p + P_SOURCE) + 108) !== m.ld32(p + P_GEN)) {\n    panic(\"stale event\");\n  }\n  const status",
            ),
        ];
        let mut patched = poll.clone();
        for (from, to) in patches {
            assert!(
                patched.contains(from),
                "reactor_afd_poll.velt has no {from:?}"
            );
            patched = patched.replace(from, to);
        }
        std::fs::write(std_copy.join("lang/hooks/reactor_afd_poll.velt"), patched).unwrap();
        if !deregisters {
            let afd = read("lang/hooks/reactor_afd.velt");
            let from = "export const DEREGISTERS: bool = true;";
            assert!(afd.contains(from));
            std::fs::write(
                std_copy.join("lang/hooks/reactor_afd.velt"),
                afd.replace(from, "export const DEREGISTERS: bool = false;"),
            )
            .unwrap();
        }
        let exe = build_std(
            dir.path(),
            &format!("churn_{deregisters}"),
            CHURN,
            Some(&std_copy),
        );
        for threads in ["1", "4"] {
            let (ok, out) = run_counted(&exe, threads);
            if deregisters {
                assert!(ok && out.starts_with("40\n"), "{threads} worker(s): {out}");
            } else {
                assert!(
                    !ok && out.contains("stale event"),
                    "no stale event without the deregistration, {threads} worker(s): {out}"
                );
            }
        }
    }
}

/// A name that does not resolve: on Windows `getaddrinfo`'s failure is a Winsock error, which
/// Rust words as an OS error ("No such host is known. (os error 11001)"), elsewhere an `EAI_*`
/// code ("failed to lookup address information: …"). Both executors word it the same, for a
/// lookup and a connect.
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
    let (rt, _) = build_and_run(dir.path(), "lookup_fails", src, false);
    let (std, _) = build_and_run(dir.path(), "lookup_fails", src, true);
    assert!(rt.contains("ENOTFOUND doesnotexist.invalid"), "{rt}");
    assert_eq!(std, rt, "the runtime's message: {rt}");
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}
