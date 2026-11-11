//! What the runtime's tokio bridge carried, in Velt under `VELT_SCHEDULER=std` (#2649 step 4):
//! the blocking pool (std/lang/hooks/sched_blocking.velt) runs velt/fs's whole-file operations,
//! standard-input reads and name lookups; UDP sockets are non-blocking sockets on the executor's
//! reactor; child processes start with `posix_spawn`, with their exit and pipes on the reactor
//! (std/process/child/child_sched.velt). A blocking job keeps the process alive until it is
//! done, even when its waiter is gone (as the runtime's exit waits for tokio's blocking threads),
//! and so does a running child. Each program is built with and without the variable, both
//! binaries run with `VELT_THREADS=4` and must print the same.

use std::io::{BufRead, BufReader, Write};
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

/// The pool runs where `#sched-thread` has threads (macOS, Linux and Windows).
fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux", windows))
}

/// Child processes and signals in Velt are POSIX's (`posix_spawn`, `sigaction`): on Windows child
/// processes stay the runtime's under `VELT_SCHEDULER=std` too (`#child-rt`, #2649), and a
/// signal's exit code (128 + 15) is a POSIX one.
fn posix() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// `src` built as `name` (with `VELT_SCHEDULER=std` when `std`): the executable.
fn build(dir: &Path, name: &str, src: &str, std: bool) -> std::path::PathBuf {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!(
        "{name}{}{}",
        if std { "_std" } else { "" },
        if cfg!(windows) { ".exe" } else { "" }
    ));
    let mut b = command(env!("CARGO_BIN_EXE_velt"));
    b.arg("build")
        .arg(&file)
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

/// Runs `exe` in `dir` with 4 workers and no input: its exit code and output.
fn run(dir: &Path, exe: &Path) -> (i32, String) {
    let o = command(exe)
        .current_dir(dir)
        .env("VELT_THREADS", "4")
        .env_remove("VELT_SCHEDULER")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    let err = String::from_utf8_lossy(&o.stderr);
    (o.status.code().unwrap_or(-1), format!("{out}{err}"))
}

/// The std seams, by a part of the mangled names of their module's functions: a std build that
/// uses one has them, so a test can tell the std path ran rather than the runtime's.
const FS: &str = "P2fsP9rt__sched";
const STDIN: &str = "P7processP9rt__sched";
const DNS: &str = "P3dnsP9rt__sched";
const UDP: &str = "P3udpP10udp__sched";
const CHILD: &str = "P5childP12child__sched";
/// A job run on the blocking pool (`sched_blocking.run`'s closure).
const POOL: &str = "sched__blockingN3run";

/// Asserts that std build `exe` has the functions of each seam in `marks` (and so went through
/// them), and the runtime build `rt` none of them.
fn assert_std_path(name: &str, exe: &Path, rt: Option<&Path>, marks: &[&str]) {
    if cfg!(windows) {
        // A PE executable names no Velt function, and UDP and child processes stay the runtime's
        // there (`#net-udp`, `#child-rt`): the other suites look for the executor's panic
        // locations instead.
        return;
    }
    let bin = std::fs::read(exe).unwrap();
    let rt_bin = rt.map(|r| std::fs::read(r).unwrap()).unwrap_or_default();
    for mark in marks {
        let has = |b: &[u8]| b.windows(mark.len()).any(|w| w == mark.as_bytes());
        assert!(
            has(&bin),
            "{name}: the std build has no `{mark}`: the std path didn't run"
        );
        assert!(!has(&rt_bin), "{name}: the runtime build has `{mark}`");
    }
}

/// Builds and runs `src` both ways: each prints `want` and exits with 0, and the std build went
/// through the seams in `marks`.
fn same_on_both(name: &str, src: &str, want: &str, marks: &[&str]) {
    let got = equal_on_both(name, src, marks);
    assert_eq!(got.as_str(), want, "{name}");
}

/// Builds and runs `src` both ways (the std build through the seams in `marks`): both exit with
/// 0 and print the same, which is returned.
fn equal_on_both(name: &str, src: &str, marks: &[&str]) -> String {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let rt = build(dir.path(), name, src, false);
    let std = build(dir.path(), name, src, true);
    assert_std_path(name, &std, Some(&rt), marks);
    let (rt_code, rt_out) = run(dir.path(), &rt);
    let (std_code, std_out) = run(dir.path(), &std);
    assert_eq!(
        (rt_code, rt_out.as_str()),
        (0, std_out.as_str()),
        "{name}: runtime vs std"
    );
    assert_eq!(std_code, 0, "{name}, std");
    std_out
}

/// Runs `exe` with 4 workers and stdin a pipe the test holds: waits for the first output line,
/// then writes `input` (and closes stdin when `close`), and waits at most 20 s for the exit. The
/// exit code (`None`: still running at the deadline, then killed) and stdout.
fn run_with_input(
    dir: &Path,
    exe: &Path,
    input: Option<&str>,
    close: bool,
) -> (Option<i32>, String) {
    let mut child = command(exe)
        .current_dir(dir)
        .env("VELT_THREADS", "4")
        .env_remove("VELT_SCHEDULER")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    out.read_line(&mut first).unwrap();
    if let Some(text) = input {
        stdin.as_mut().unwrap().write_all(text.as_bytes()).unwrap();
        stdin.as_mut().unwrap().flush().unwrap();
    }
    if close {
        stdin = None;
    }
    let reader = std::thread::spawn(move || {
        let mut rest = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut rest);
        rest
    });
    let start = Instant::now();
    let code = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st.code();
        }
        // timing: a hang guard, not an order: a run that ends in time is the result.
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        // timing: polls for the exit the hang guard above bounds.
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(stdin);
    let rest = reader.join().unwrap();
    (code, format!("{first}{rest}").replace("\r\n", "\n"))
}

#[test]
fn file_operations_from_every_worker_finish_on_the_pool() {
    if !has_threads() {
        return;
    }
    // 64 tasks, pinned across the workers, each write, append, stat and read their own file at
    // once; the jobs overlap on the pool's threads and every result comes back to its task.
    same_on_both(
        "fs_many",
        r#"import { appendFile, exists, mkdir, readDir, readDirEntries, readFile, readFileBytes,
  remove, rename, stat, writeFile } from "velt/fs";
import { workerCount } from "velt/task";

async function one(dir: string, i: i64): Promise<string> {
  const f = `${dir}/f${i}.txt`;
  await writeFile(f, `hello ${i}`);
  await appendFile(f, `!`);
  const s = await stat(f);
  await rename(f, `${dir}/g${i}.txt`);
  const text = await readFile(`${dir}/g${i}.txt`);
  const bytes = await readFileBytes(`${dir}/g${i}.txt`);
  return `${text}:${s.size}:${bytes.length}`;
}

async function main() {
  const dir = "pool_dir";
  if (await exists(dir)) {
    await remove(dir, { recursive: true });
  }
  await mkdir(dir);
  const tasks: Promise<string>[] = [];
  for (let i: i64 = 0; i < 64; i++) {
    tasks.push(spawn(one(dir, i), { worker: i % workerCount() }));
  }
  const all = await Promise.all(tasks);
  let right = 0;
  for (let i = 0; i < all.length; i++) {
    const want = `hello ${i}!`;
    if (all[i] == `${want}:${want.length}:${want.length}`) right++;
  }
  console.log(`right ${right}`);
  console.log(`entries ${(await readDir(dir)).length} ${(await readDirEntries(dir)).length}`);
  let missing = "";
  try {
    await readFile(`${dir}/none.txt`);
  } catch (e) {
    missing = "missing file throws";
  }
  console.log(missing);
  await remove(dir, { recursive: true });
  console.log(`gone ${!(await exists(dir))}`);
}
"#,
        "right 64\nentries 64 64\nmissing file throws\ngone true\n",
        &[FS, POOL],
    );
}

/// The program of the stdin tests: prints "waiting", then (`race`) gives up its read for a
/// promise that is settled already, or waits for the line.
fn stdin_program(race: bool) -> String {
    let wait = if race {
        "const got = await Promise.race<string | null>([readLine(), Promise.resolve<string | null>(\"gave up\")]);"
    } else {
        "const got = await readLine();"
    };
    format!(
        r#"import {{ readLine }} from "velt/process";

async function main() {{
  console.log("waiting");
  {wait}
  console.log(`got ${{got}}`);
}}
"#
    )
}

#[test]
fn a_pending_stdin_read_keeps_the_process_alive_until_its_line_comes() {
    if !has_threads() {
        return;
    }
    // The line is written only once the program printed "waiting", so the read is pending on the
    // pool's thread with nothing else alive: the program must not end before it comes.
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for std in [false, true] {
        let exe = build(dir.path(), "stdin_wait", &stdin_program(false), std);
        if std {
            assert_std_path("stdin_wait", &exe, None, &[STDIN, POOL]);
        }
        let got = run_with_input(dir.path(), &exe, Some("hello\n"), false);
        assert_eq!(
            got,
            (Some(0), "waiting\ngot hello\n".to_string()),
            "std: {std}"
        );
    }
}

#[test]
fn a_stdin_read_nobody_waits_for_still_ends_with_its_line() {
    if !has_threads() {
        return;
    }
    // The read loses a race and is dropped while its job blocks on the pipe; `main` ends. The job
    // keeps the process alive until its line comes, and the process ends once it has: the job's
    // end wakes worker 0 to look again.
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for std in [false, true] {
        let exe = build(dir.path(), "stdin_race", &stdin_program(true), std);
        if std {
            assert_std_path("stdin_race", &exe, None, &[STDIN, POOL]);
        }
        let got = run_with_input(dir.path(), &exe, Some("late\n"), false);
        assert_eq!(
            got,
            (Some(0), "waiting\ngot gave up\n".to_string()),
            "std: {std}"
        );
    }
}

#[test]
fn stdin_lines_and_the_rest_come_through_the_pool_in_order() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let src = r#"import { readAll, readLine } from "velt/process";

async function main() {
  console.log("ready");
  const a = await readLine();
  const b = await readLine();
  const rest = await readAll();
  console.log(`${a}|${b}|${rest.length}|${await readLine()}`);
}
"#;
    for std in [false, true] {
        let exe = build(dir.path(), "stdin_lines", src, std);
        if std {
            assert_std_path("stdin_lines", &exe, None, &[STDIN, POOL]);
        }
        let got = run_with_input(dir.path(), &exe, Some("one\ntwo\nthree\nfour\n"), true);
        assert_eq!(
            got,
            (Some(0), "ready\none|two|11|null\n".to_string()),
            "std: {std}"
        );
    }
}

#[test]
fn udp_datagrams_cross_workers_and_names_resolve_on_the_pool() {
    if !has_threads() {
        return;
    }
    // Under std the sockets are non-blocking ones on the reactor and `lookup` runs `getaddrinfo`
    // on the blocking pool. An echo task pinned to the last worker answers 200 datagrams sent
    // from worker 0 (each receive waits for readiness first), and a name resolves on the pool.
    same_on_both(
        "udp_workers",
        r#"import { bindUdp, UdpSocket } from "velt/net/udp";
import { lookup } from "velt/net/dns";
import { workerCount } from "velt/task";

async function echo(s: UdpSocket, n: i64): Promise<i64> {
  let bytes: i64 = 0;
  for (let i = 0; i < n; i++) {
    const d = await s.recvFrom();
    bytes += d.data.length;
    await s.sendTo(d.data, d.addr);
  }
  return bytes;
}

async function main() {
  const server = await bindUdp("127.0.0.1:0");
  const client = await bindUdp("127.0.0.1:0");
  const e = spawn(echo(server, 200), { worker: workerCount() - 1 });
  let right = 0;
  for (let i = 0; i < 200; i++) {
    const text = `datagram ${i}`;
    await client.sendTextTo(text, `127.0.0.1:${server.port}`);
    const back = await client.recvFrom();
    if (back.text() == text && back.addr == `127.0.0.1:${server.port}`) right++;
  }
  console.log(`echoed ${right} ${await e}`);
  const names = await lookup("localhost");
  console.log(`localhost ${names.includes("127.0.0.1") || names.includes("::1")}`);
  server.close();
  client.close();
}
"#,
        "echoed 200 2290\nlocalhost true\n",
        &[UDP, DNS, POOL],
    );
}

#[test]
fn a_receive_with_a_limit_keeps_that_many_bytes() {
    if !has_threads() {
        return;
    }
    // recvFrom(max) under std reads into a buffer of `max` bytes: the datagram's first `max`
    // bytes come back, a limit above 64 KiB or none keeps it whole, as with the runtime.
    same_on_both(
        "udp_max",
        r#"import { bindUdp } from "velt/net/udp";

async function main() {
  const a = await bindUdp("127.0.0.1:0");
  const b = await bindUdp("127.0.0.1:0");
  const text = "0123456789".repeat(10);
  await a.sendTextTo(text, `127.0.0.1:${b.port}`);
  const cut = await b.recvFrom(7);
  await a.sendTextTo(text, `127.0.0.1:${b.port}`);
  const whole = await b.recvFrom();
  await a.sendTextTo(text, `127.0.0.1:${b.port}`);
  const big = await b.recvFrom(100000);
  console.log(`${cut.text()} ${whole.data.length} ${big.data.length}`);
  a.close();
  b.close();
}
"#,
        "0123456 100 100\n",
        &[UDP],
    );
}

#[test]
fn a_job_runs_when_the_pool_can_start_no_thread() {
    if !has_threads() {
        return;
    }
    // With no pool thread running and none able to start (an OS refusing threads, as for a user
    // at their process limit), the job that asked for one used to wait in the queue for good,
    // holding its keep-alive reference: the program never ended. Now the thread that queued it
    // runs the queue. White-box, in a copy of std whose `startDetached` always fails (an
    // `RLIMIT_NPROC` limit would stop the runtime's own threads first; thread_win32.velt's on
    // Windows). The job is then done before the poll that queued it ends, every time: its result
    // still waits for a wake, so a settled promise wins a race against it (#2820), as in Node; it
    // used to lose every time.
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let std_copy = dir.path().join("std");
    copy_tree(&manifest().join("../../std"), &std_copy);
    let thread = std_copy.join(if cfg!(windows) {
        "lang/hooks/thread_win32.velt"
    } else {
        "lang/hooks/thread_posix.velt"
    });
    let text = std::fs::read_to_string(&thread)
        .unwrap()
        .replace("\r\n", "\n");
    let from = "export function startDetached(start: usize, arg: usize): bool {\n";
    assert!(
        text.contains(from),
        "{} has no startDetached",
        thread.display()
    );
    std::fs::write(
        &thread,
        text.replace(from, &format!("{from}  return false;\n")),
    )
    .unwrap();
    let file = dir.path().join("no_threads.velt");
    std::fs::write(
        &file,
        r#"import { readFile, writeFile } from "velt/fs";

async function main() {
  await writeFile("nt.txt", "pool or not");
  console.log(await readFile("nt.txt"));
  console.log(await Promise.race<string>([readFile("nt.txt"), Promise.resolve("first")]));
}
"#,
    )
    .unwrap();
    let exe = dir.path().join(if cfg!(windows) { "nt.exe" } else { "nt" });
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&exe)
        .current_dir(dir.path())
        .env("VELT_STD", &std_copy)
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut child = command(&exe)
        .current_dir(dir.path())
        .env("VELT_THREADS", "4")
        .env_remove("VELT_SCHEDULER")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut text);
        text
    });
    let start = Instant::now();
    let code = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st.code();
        }
        // timing: a hang guard, not an order: a run that ends in time is the result.
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        // timing: polls for the exit the hang guard above bounds.
        std::thread::sleep(Duration::from_millis(20));
    };
    let got = (code, reader.join().unwrap());
    assert_eq!(got, (Some(0), "pool or not\nfirst\n".to_string()));
}

#[test]
fn child_processes_from_every_worker_run_and_report_their_exit() {
    if !posix() {
        return;
    }
    // The program runs itself as its children (no shell tools needed). 24 `exec`s at once from
    // tasks pinned across the workers (stdin fed, stdout and stderr drained, exit reaped), a
    // spawned child talked to through its pipes, one killed by a signal (128 + 15), a child
    // whose `PATH` names the program's directory, and a program that doesn't exist.
    same_on_both(
        "children",
        r#"import { exec, spawn as start } from "velt/process/child";
import { argv, args, readAll, readLine } from "velt/process";
import { workerCount } from "velt/task";

async function child(mode: string): Promise<i32> {
  if (mode == "echo") {
    const input = await readAll();
    console.log(input.toUpperCase());
    console.error(`err ${input.length}`);
    return input.length as i32;
  }
  if (mode == "wait") {
    const line = await readLine();
    return 0;
  }
  return 1;
}

async function one(self: string, i: i64): Promise<string> {
  const r = await exec(self, ["echo"], { input: `job${i}` });
  return `${r.code}:${r.stdout.trim()}:${r.stderr.trim()}`;
}

async function parent(self: string) {
  const tasks: Promise<string>[] = [];
  for (let i: i64 = 0; i < 24; i++) {
    tasks.push(spawn(one(self, i), { worker: i % workerCount() }));
  }
  const all = await Promise.all(tasks);
  let right = 0;
  for (let i = 0; i < all.length; i++) {
    const n = `job${i}`.length;
    if (all[i] == `${n}:JOB${i}:err ${n}`) right++;
  }
  console.log(`exec ${right}`);

  const p = start(self, ["echo"], { stdin: "pipe", stdout: "pipe", stderr: "pipe" });
  await p.write("piped ");
  await p.write("text");
  await p.closeStdin();
  let out = "";
  let chunk = await p.readStdout();
  while (chunk != "") {
    out += chunk;
    chunk = await p.readStdout();
  }
  console.log(`${out.trim()} ${(await p.readStderr()).trim()} exit ${await p.wait()} ${p.exitCode}`);
  p.close();

  const k = start(self, ["wait"], { stdin: "pipe" });
  console.log(`running ${k.exitCode == null}`);
  k.kill();
  console.log(`killed ${await k.wait()}`);
  k.close();

  const slash = self.lastIndexOf("/");
  const env = new Map<string, string>();
  env.set("PATH", self.slice(0, slash));
  const viaPath = await exec(self.slice(slash + 1), ["echo"], { input: "x", env: env });
  console.log(`path ${viaPath.code}`);

  try {
    await exec("velt-no-such-program");
  } catch (err) {
    console.log(`missing ${err.code} ${err.message.startsWith("spawn velt-no-such-program: ")}`);
  }
}

async function main(): Promise<i32> {
  const a = args();
  if (a.length > 0) {
    return await child(a[0]);
  }
  await parent(argv()[0].clone());
  return 0;
}
"#,
        "exec 24\nPIPED TEXT err 10 exit 10 10\nrunning true\nkilled 143\npath 1\nmissing ENOENT true\n",
        &[CHILD, STDIN, POOL],
    );
}

#[test]
fn a_read_of_a_childs_silent_pipe_leaves_its_worker_free() {
    if !posix() {
        return;
    }
    // The parent starts reading the child's stdout before it writes the line the child waits
    // for: the read must find the pipe empty and wait for readiness. On a blocking pipe end it
    // would hold the worker (here the one the write runs on) until the child wrote: never.
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(
        dir.path(),
        "silent_pipe",
        r#"import { spawn as start } from "velt/process/child";
import { argv, args, readLine } from "velt/process";

async function main(): Promise<i32> {
  if (args().length > 0) {
    console.log(`got ${await readLine()}`);
    return 0;
  }
  console.log("started");
  const k = start(argv()[0].clone(), ["line"], { stdin: "pipe", stdout: "pipe" });
  const out = k.readStdout();
  await k.write("go\n");
  await k.closeStdin();
  console.log((await out).trim());
  console.log(`exit ${await k.wait()}`);
  k.close();
  return 0;
}
"#,
        true,
    );
    assert_std_path("silent_pipe", &exe, None, &[CHILD]);
    let got = run_with_input(dir.path(), &exe, None, false);
    assert_eq!(got, (Some(0), "started\ngot go\nexit 0\n".to_string()));
}

#[test]
fn a_nul_in_a_childs_working_directory_fails_its_start() {
    if !posix() {
        return;
    }
    // The cwd is a C string too: with a NUL in it the start fails, as the runtime's does, where
    // the std path used to run the child in this program's directory.
    let out = equal_on_both(
        "cwd_nul",
        r#"import { exec } from "velt/process/child";
import { argv, args } from "velt/process";

async function main(): Promise<i32> {
  if (args().length > 0) {
    console.log("child ran");
    return 0;
  }
  try {
    const r = await exec(argv()[0].clone(), ["x"], { cwd: "/usr\0x" });
    console.log(`ran ${r.code} ${r.stdout.trim()}`);
  } catch (err) {
    console.log(`throws ${err.code} ${err.message.endsWith(": nul byte found in provided data")}`);
  }
  return 0;
}
"#,
        &[CHILD],
    );
    assert!(
        out.starts_with("throws ") && out.ends_with(" true\n"),
        "{out}"
    );
}

#[test]
fn socket_and_name_errors_read_as_the_runtimes() {
    if !posix() {
        return;
    }
    // An IPv4 socket sending to an IPv6 address (EAFNOSUPPORT: the runtime's UNKNOWN, as Rust
    // maps no kind to it) and a name with a NUL (the message of Rust's C string conversion).
    let out = equal_on_both(
        "net_errors",
        r#"import { bindUdp } from "velt/net/udp";
import { lookup } from "velt/net/dns";

async function main() {
  const a = await bindUdp("127.0.0.1:0");
  try {
    await a.sendTextTo("hi", "[::1]:9");
    console.log("sent");
  } catch (e) {
    console.log(`send ${e.code}`);
  }
  a.close();
  try {
    await lookup("a\0b");
  } catch (e) {
    console.log(`lookup ${e.code} ${e.message.replace("\0", " ")}`);
  }
}
"#,
        &[UDP, DNS, POOL],
    );
    assert!(out.contains("an unexpected NUL byte"), "{out}");
}

/// Copies directory `from` into `to` (made), recursively.
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

#[test]
fn a_raised_signal_comes_through_the_self_pipe() {
    if !posix() {
        return;
    }
    // Signals have no std API yet (only the `velt dev` host watches them), so this is a
    // white-box test of std/lang/hooks/sched_signal.velt, in a copy of std (a test file that
    // imports the executor builds only under `VELT_SCHEDULER=std`, and `velt test std/` runs
    // every test file in the default build).
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let std_copy = dir.path().join("std");
    copy_tree(&manifest().join("../../std"), &std_copy);
    let test = std_copy.join("lang/hooks/sched_signal_raise.test.velt");
    std::fs::write(
        &test,
        r##"import { describe, expect, it } from "velt/test";
import * as sg from "#sched-signal";
import * as signal from "./sched_signal";

declare c("c") function kill(pid: i32, sig: i32): i32;
declare c("c") function getpid(): i32;

describe("signals", () => {
  it("delivers a raised signal to next(), each time", async () => {
    expect(signal.watch(sg.SIGUSR1)).toBe(true);
    for (let i = 0; i < 3; i++) {
      // SAFETY: plain system calls.
      unsafe {
        kill(getpid(), sg.SIGUSR1);
      }
      expect(await signal.next()).toBe(sg.SIGUSR1);
    }
  });
});
"##,
    )
    .unwrap();
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("test")
        .arg(&test)
        .current_dir(dir.path())
        .env("VELT_STD", &std_copy)
        .env("VELT_SCHEDULER", "std")
        .env("VELT_THREADS", "4")
        .output()
        .unwrap();
    let out = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success() && out.contains("1 passed"), "{out}");
}
