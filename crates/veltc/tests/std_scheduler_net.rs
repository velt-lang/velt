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
    let dir = test_dir::TestDir::new();
    same_on_both_in(dir.path(), name, src, want);
}

/// [`same_on_both`] in folder `dir` (which may hold packages the program uses).
fn same_on_both_in(dir: &Path, name: &str, src: &str, want: &str) {
    runtime_support::build_native_runtime(manifest());
    let (rt, rt_bin) = build_and_run(dir, name, src, false);
    let (std, std_bin) = build_and_run(dir, name, src, true);
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
    );
}
