//! Child processes on Windows (std/process/child/child_win32.velt), where they differ from macOS
//! and Linux: batch files are refused however the name is written (Node's CVE-2024-27980 rule;
//! Windows drops trailing dots and spaces from a name, and `cmd.exe` would read an argument as
//! commands), `execSync` finds and checks a program as `exec` does, and running children hold no
//! thread of the executor's blocking pool. The portable behaviour is in the goldens
//! (tests/golden/std/child_*.velt).
#![cfg(windows)]

use std::path::Path;
use std::process::Stdio;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Builds `src` in `dir` and runs it there with `args`: its exit code and output.
fn build_and_run(dir: &Path, name: &str, src: &str, args: &[&str]) -> (Option<i32>, String) {
    runtime_support::build_native_runtime(manifest());
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!("{name}.exe"));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
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
    let o = command(&exe)
        .args(args)
        .current_dir(dir)
        // The lookup looks in the working directory first unless this is set (as some shells
        // and tools set it); the tests expect Windows' default.
        .env_remove("NoDefaultCurrentDirectoryInExePath")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    let err = String::from_utf8_lossy(&o.stderr);
    (o.status.code(), format!("{out}{err}"))
}

#[test]
fn a_batch_file_is_refused_however_its_name_is_written() {
    // Each name opens x.bat or x.cmd (Windows drops a name's trailing dots and spaces), directly,
    // in the working directory, or through `PATH`. Started, the batch file would run the
    // argument's `echo INJECTED` through cmd.exe. Node 22 refuses every one with EINVAL.
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(
        dir.path(),
        "bat",
        r#"import { exec, execSync } from "velt/process/child";
import { writeFileSync } from "velt/fs";
import { cwd } from "velt/process";

async function main(): Promise<i32> {
  writeFileSync("x.bat", "@echo ran %*\r\n");
  writeFileSync("x.cmd", "@echo ran %*\r\n");
  const injecting = "a\"&echo INJECTED&\"";
  const here = cwd();
  const path = new Map<string, string>();
  path.set("PATH", here);
  const names = ["x.bat", "X.BAT", "x.bat.", "x.bat ", "x.bat. .", "x.cmd.", ".\\x.bat.", `${here}\\x.bat.`];
  for (const name of names) {
    let got = "";
    try {
      const r = await exec(name, [injecting]);
      got = `ran ${r.stdout.trim()}`;
    } catch (e) {
      got = e.code;
    }
    let gotSync = "";
    try {
      const r = execSync(name, [injecting]);
      gotSync = `ran ${r.stdout.trim()}`;
    } catch (e) {
      gotSync = e.code;
    }
    console.log(`${JSON.stringify(name)} ${got} ${gotSync}`);
  }
  for (const name of ["x.bat.", "x.cmd "]) {
    try {
      const r = await exec(name, [injecting], { env: path, cwd: "C:\\" });
      console.log(`PATH ${JSON.stringify(name)} ran ${r.stdout.trim()}`);
    } catch (e) {
      console.log(`PATH ${JSON.stringify(name)} ${e.code}`);
    }
  }
  return 0;
}
"#,
        &[],
    );
    assert_eq!(code, Some(0), "{out}");
    let want = r#""x.bat" EINVAL EINVAL
"X.BAT" EINVAL EINVAL
"x.bat." EINVAL EINVAL
"x.bat " EINVAL EINVAL
"x.bat. ." EINVAL EINVAL
"x.cmd." EINVAL EINVAL
".\\x.bat." EINVAL EINVAL
"#;
    assert!(out.starts_with(want), "{out}");
    assert!(!out.contains("INJECTED"), "{out}");
    assert!(
        out.ends_with("PATH \"x.bat.\" EINVAL\nPATH \"x.cmd \" EINVAL\n"),
        "{out}"
    );
}

#[test]
fn execsync_finds_a_program_as_exec_does() {
    // A program in the child's working directory, given by a bare name: libuv's lookup (Node's)
    // looks there first; Rust's `Command`, which `execSync` ran once, would not.
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(
        dir.path(),
        "finder",
        r#"import { exec, execSync } from "velt/process/child";
import { argv, args, cwd } from "velt/process";
import { copyFileSync, existsSync, mkdirSync } from "velt/fs";

async function main(): Promise<i32> {
  if (args().length > 0) {
    console.log("found");
    return 0;
  }
  mkdirSync("sub");
  copyFileSync(argv()[0], "sub\\helper.exe");
  // A debug build loads the runtime's DLL from beside it or from its working directory.
  if (existsSync("velt_rt_shared.dll")) {
    copyFileSync("velt_rt_shared.dll", "sub\\velt_rt_shared.dll");
  }
  const opts = { cwd: `${cwd()}\\sub` };
  console.log(`exec ${(await exec("helper", ["x"], opts)).stdout.trim()}`);
  console.log(`execSync ${execSync("helper", ["x"], opts).stdout.trim()}`);
  return 0;
}
"#,
        &[],
    );
    assert_eq!(
        (code, out.as_str()),
        (Some(0), "exec found\nexecSync found\n")
    );
}

#[test]
fn running_children_leave_the_blocking_pool_free() {
    // 600 children wait on their stdin while a file is read on the blocking pool (512 threads at
    // most). A wait on the pool per child would leave the read queued behind them until they
    // end; it must finish while they run.
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(
        dir.path(),
        "starve",
        r#"import { spawn, ChildProcess } from "velt/process/child";
import { readFile, writeFileSync } from "velt/fs";

async function main(): Promise<i32> {
  writeFileSync("data.txt", "contents");
  const kids: ChildProcess[] = [];
  for (let i = 0; i < 600; i++) {
    kids.push(spawn("sort", [], { stdin: "pipe", stdout: "ignore" }));
  }
  // The read is either done well before the children end, or stuck until they do: the timer
  // only bounds the wait for the broken case. timing: a 20 s bound on a read of a few bytes.
  const timer = new Promise<string>((resolve) => {
    setTimeout(() => resolve("stuck"), 20000);
  });
  const read = readFile("data.txt").then((s: string): string => `read ${s}`);
  console.log(await Promise.race<string>([read, timer]));
  for (const k of kids) {
    await k.closeStdin();
  }
  let ended = 0;
  for (const k of kids) {
    if ((await k.wait()) == 0) {
      ended++;
    }
    k.close();
  }
  console.log(`${ended} ended`);
  return 0;
}
"#,
        &[],
    );
    assert_eq!(
        (code, out.as_str()),
        (Some(0), "read contents\n600 ended\n")
    );
}

#[test]
fn many_writes_in_flight_hold_one_pool_thread() {
    // 2000 writes of a 1 KiB line not awaited one by one, while stdout is read: the child echoes
    // each line, so the writes block on a full pipe until the read drains the child. Waiting for
    // their turn on pool threads, the writes filled all 512 and the read never ran.
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(
        dir.path(),
        "flood",
        r#"import { spawn } from "velt/process/child";
import { argv, args, readLine } from "velt/process";

async function main(): Promise<i32> {
  if (args().length > 0) {
    let line = await readLine();
    while (line != null) {
      console.log(line);
      line = await readLine();
    }
    return 0;
  }
  const p = spawn(argv()[0].clone(), ["echo"], { stdin: "pipe", stdout: "pipe" });
  const line = "x".repeat(1023);
  const writes: Promise<void>[] = [];
  for (let i = 0; i < 2000; i++) {
    writes.push(p.write(`${line}\n`));
  }
  const fed = Promise.all<void>(writes).then((): string => "fed");
  const drained = (async (): Promise<string> => {
    let n = 0;
    let chunk = await p.readStdout();
    while (chunk != "") {
      n += chunk.replaceAll("\r", "").length;
      if (n >= 2000 * 1024) {
        return `read ${n}`;
      }
      chunk = await p.readStdout();
    }
    return `ended at ${n}`;
  })();
  // timing: a 30 s bound on what takes well under a second; it only ends the broken case.
  const timer = new Promise<string>((resolve) => {
    setTimeout(() => resolve("stuck"), 30000);
  });
  const got = await Promise.race<string>([
    Promise.all<string>([fed, drained]).then((r: string[]): string => r.join(" ")),
    timer,
  ]);
  console.log(got);
  if (got == "stuck") {
    // The pool is wedged: nothing more would finish.
    process.exit(1);
  }
  await p.closeStdin();
  console.log(`exit ${await p.wait()}`);
  p.close();
  return 0;
}
"#,
        &[],
    );
    assert_eq!(
        (code, out.as_str()),
        (Some(0), "fed read 2048000\nexit 0\n")
    );
}

#[test]
fn a_failed_windows_call_fails_execsync() {
    // `$VELT_TEST_EXEC_SYNC_FAIL` makes one step's Windows call fail with ERROR_NOT_ENOUGH_MEMORY
    // (std/process/child/exec_sync_win32.velt). Each failure throws, as Node's `spawnSync` does
    // when libuv reports a pipe's error; none gives a run's partial output as its result. A pipe's
    // event fails before the child starts (no marker file); a read, a write or a wait fails with
    // the child ended at once (it would run 40 s); the exit code's read after it ended.
    let dir = test_dir::TestDir::new();
    let (code, out) = build_and_run(
        dir.path(),
        "failing",
        r#"import { execSync, SpawnError } from "velt/process/child";
import { existsSync } from "velt/fs";
import { removeEnv, setEnv } from "velt/process";

function run(step: string, line: string, input: string): string {
  setEnv("VELT_TEST_EXEC_SYNC_FAIL", step);
  const started = Date.now();
  let got = "";
  try {
    const r = execSync("cmd.exe", ["/d", "/c", line], { input: input });
    got = `ran ${r.code} [${r.stdout.trim()}]`;
  } catch (err) {
    got = err instanceof SpawnError ? `${err.code} ${err.message}` : `other ${err.message}`;
  }
  removeEnv("VELT_TEST_EXEC_SYNC_FAIL");
  const waited = Date.now() - started < 20000 ? "" : " (waited for the child)";
  return `${step}: ${got}${waited}`;
}

function main(): i32 {
  const long = "ping -n 40 127.0.0.1 >nul & echo out";
  console.log(run("event", "echo ran> marker.txt & echo out", ""));
  console.log(`marker: ${existsSync("marker.txt")}`);
  console.log(run("event", "echo out", "input"));
  console.log(run("read", long, ""));
  console.log(run("write", long, "input"));
  console.log(run("wait", long, ""));
  console.log(run("exit", "echo out", ""));
  console.log(run("", "echo out", "input"));
  return 0;
}
"#,
        &[],
    );
    assert_eq!(
        (code, out.as_str()),
        (
            Some(0),
            "event: ENOMEM spawnSync cmd.exe ENOMEM\n\
             marker: false\n\
             event: ENOMEM spawnSync cmd.exe ENOMEM\n\
             read: ENOMEM spawnSync cmd.exe ENOMEM\n\
             write: ENOMEM spawnSync cmd.exe ENOMEM\n\
             wait: ENOMEM spawnSync cmd.exe ENOMEM\n\
             exit: ENOMEM spawnSync cmd.exe ENOMEM\n\
             : ran 0 [out]\n"
        )
    );
}
