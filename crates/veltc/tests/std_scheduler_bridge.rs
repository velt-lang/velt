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

/// The pool runs where `#sched-thread` has threads (macOS and Linux for now).
fn has_threads() -> bool {
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

/// Builds and runs `src` both ways: each prints `want` and exits with 0.
fn same_on_both(name: &str, src: &str, want: &str) {
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for std in [false, true] {
        let exe = build(dir.path(), name, src, std);
        let (code, out) = run(dir.path(), &exe);
        assert_eq!((code, out.as_str()), (0, want), "{name}, std: {std}");
    }
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
    );
}

#[test]
fn child_processes_from_every_worker_run_and_report_their_exit() {
    if !has_threads() {
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
    );
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
    if !has_threads() {
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
