//! `catchPanic` under the executor in Velt (std/task/catch_sched.velt, #2667; design:
//! docs/internals/design/unwinding.md "The executor in Velt"): no runtime catch, generated code
//! catches Velt's exception with `catchUnwind`. The executor polls only catching promises (and
//! the roots of tasks spawned while one runs) through the catch; a panicked one raises the panic
//! again in whoever awaits or joins it, on whichever worker that is. A program that never calls
//! `catchPanic` has no catch at all.

use std::path::{Path, PathBuf};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Where the executor in Velt catches: a `VELT_SCHEDULER=std` program builds on unix only (no
/// reactor in Velt on Windows yet), and generated code catches with the Itanium unwinder (macOS
/// and Linux; Windows' structured exceptions are not built yet).
fn catches_here() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux"))
}

/// `file` built with `VELT_SCHEDULER=std` (from its folder, so a golden finds its packages) into
/// `exe`.
fn build_std(file: &Path, exe: &Path) {
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(file)
        .arg("-o")
        .arg(exe)
        .current_dir(file.parent().unwrap())
        .env("VELT_SCHEDULER", "std")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{}: {}",
        file.display(),
        String::from_utf8_lossy(&o.stderr)
    );
}

/// Runs `exe` from `dir` with `threads` workers: its exit code, stdout and stderr.
fn run(exe: &Path, dir: &Path, threads: &str) -> (i32, String, String) {
    run_rescue(exe, dir, threads, None)
}

/// `run`, with `VELT_RESCUE` set to `rescue` (unset when `None`: one worker then runs solo, with
/// no monitor and no lock; sched_task.velt `E_SOLO`).
fn run_rescue(
    exe: &Path,
    dir: &Path,
    threads: &str,
    rescue: Option<&str>,
) -> (i32, String, String) {
    let mut c = command(exe);
    c.current_dir(dir)
        .env("VELT_THREADS", threads)
        .env_remove("VELT_SCHEDULER")
        .env_remove("VELT_RESCUE");
    if let Some(r) = rescue {
        c.env("VELT_RESCUE", r);
    }
    let o = c.output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&o.stderr).replace("\r\n", "\n"),
    )
}

fn exe_name(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }))
}

/// The goldens that catch panics (`catchPanic` directly, or http-server's 500 on a panicking
/// handler), which ended with 101 under the executor in Velt before it caught.
const GOLDENS: &[&str] = &[
    "std/catch_panic",
    "std/catch_panic_in_clone",
    "std/catch_panic_in_clone_cycle",
    "std/catch_panic_promises",
    "std/catch_panic_with",
    "std/shared_fn_panic_caught",
    "std/shared_fn_panic_caught_generator_interface",
    "std/shared_fn_panic_caught_resource_any",
    "std/shared_fn_panic_caught_resource_interface",
    "std/shared_fn_spawn_after_caught_transfer_panic",
    "std/shared_fn_walk_after_caught_transfer_panic",
    "std/simd_half_range",
    "std/http_server/http_request_after_handler",
    "std/http_server/http_request_body_after_response",
];

#[test]
fn the_goldens_that_catch_panics_pass_under_the_executor_in_velt() {
    if !catches_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let golden = manifest().join("../../tests/golden");
    let mut failures = Vec::new();
    for name in GOLDENS {
        let file = golden.join(format!("{name}.velt"));
        let exe = exe_name(dir.path(), &name.replace('/', "_"));
        build_std(&file, &exe);
        let want_out = std::fs::read_to_string(file.with_extension("out"))
            .unwrap()
            .replace("\r\n", "\n");
        let want_code = std::fs::read_to_string(file.with_extension("code"))
            .map(|c| c.trim().parse::<i32>().unwrap())
            .unwrap_or(0);
        let want_err = std::fs::read_to_string(file.with_extension("stderr")).unwrap_or_default();
        for threads in ["1", "4"] {
            let (code, out, err) = run(&exe, file.parent().unwrap(), threads);
            let missing: Vec<&str> = want_err
                .lines()
                .filter(|l| !l.is_empty() && !err.contains(l))
                .collect();
            if code != want_code || out != want_out || !missing.is_empty() {
                failures.push(format!(
                    "{name} (VELT_THREADS={threads}): exit {code} (want {want_code})\n\
                     --- stdout ---\n{out}--- want ---\n{want_out}--- stderr ---\n{err}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn a_panic_in_a_task_on_another_worker_reaches_its_joiners_catch_panic() {
    if !catches_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("joined.velt");
    std::fs::write(
        &file,
        r#"import { catchPanic, currentWorker, PanicError } from "velt/task";

let ran = "";

async function work(n: i64): Promise<i64> {
  await yieldNow();
  ran = `${currentWorker()}`;
  const xs: i64[] = [1, 2];
  return xs[n]!;
}

// The task is spawned while the guarded promise's catching poll runs, on worker 1; the joiner
// (the guarded promise) runs on worker 0.
async function joins(n: i64): Promise<i64> {
  return await spawn(work(n), { worker: 1 });
}

async function report(n: i64): Promise<string> {
  try {
    return `result ${await catchPanic(() => joins(n))}`;
  } catch (e) {
    const m = (e as Error).message.split(" at ")[0];
    return `caught ${e instanceof PanicError} on worker ${currentWorker()}: ${m}`;
  }
}

async function main() {
  console.log(await report(1));
  console.log(await report(7));
  let caught = 0;
  for (let i = 0; i < 100; i++) {
    if ((await report(9)).startsWith("caught true on worker 0")) caught++;
  }
  console.log(`caught ${caught} of 100, the task ran on worker ${ran}`);
  console.log(await report(0));
}
"#,
    )
    .unwrap();
    let exe = exe_name(dir.path(), "joined");
    build_std(&file, &exe);
    let (code, out, err) = run(&exe, dir.path(), "4");
    assert_eq!(
        (code, out.as_str()),
        (
            0,
            "result 2\n\
             caught true on worker 0: index out of bounds: the len is 2 but the index is 7\n\
             caught 100 of 100, the task ran on worker 1\n\
             result 1\n"
        ),
        "stderr: {err}"
    );
    // Each panic is printed once, where it happened (not again where it is raised again).
    assert_eq!(err.matches("panic: ").count(), 101, "stderr: {err}");
}

#[test]
fn a_panic_in_a_task_on_a_solo_worker_rejects_its_join_as_with_other_threads() {
    if !catches_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("solo.velt");
    std::fs::write(
        &file,
        r#"import { catchPanic, PanicError } from "velt/task";

async function at(n: i64, wait: bool): Promise<i64> {
  if (wait) {
    await yieldNow();
  }
  const xs: i64[] = [1, 2];
  return xs[n]!;
}

// The task panics before its join handle is polled.
async function joinAfter(n: i64): Promise<i64> {
  const h = spawn(at(n, false));
  await yieldNow();
  await yieldNow();
  return await h;
}

// The joiner waits in the join slot when the task panics.
async function joinBefore(n: i64): Promise<i64> {
  return await spawn(at(n, true));
}

async function joinAll(n: i64): Promise<i64> {
  const xs = await Promise.all([spawn(at(0, true)), spawn(at(n, true)), spawn(at(1, false))]);
  return xs[0] + xs[1] + xs[2];
}

// Nobody joins the task: its panic is printed and ends there.
async function unjoined(n: i64): Promise<i64> {
  spawn(at(n, false));
  await yieldNow();
  await yieldNow();
  return 3;
}

async function report(f: () => Promise<i64>): Promise<string> {
  try {
    return `result ${await catchPanic(f)}`;
  } catch (e) {
    return `caught ${e instanceof PanicError}: ${(e as Error).message.split(" at ")[0]}`;
  }
}

async function main() {
  console.log(await report(() => joinAfter(1)));
  console.log(await report(() => joinAfter(7)));
  console.log(await report(() => joinBefore(0)));
  console.log(await report(() => joinBefore(9)));
  console.log(await report(() => joinAll(1)));
  console.log(await report(() => joinAll(4)));
  console.log(await report(() => unjoined(6)));
  let caught = 0;
  for (let i = 0; i < 100; i++) {
    if ((await report(() => joinBefore(3))).startsWith("caught true")) caught++;
  }
  console.log(`caught ${caught} of 100`);
}
"#,
    )
    .unwrap();
    let exe = exe_name(dir.path(), "solo");
    build_std(&file, &exe);
    let oob = "index out of bounds: the len is 2 but the index is";
    let want = format!(
        "result 2\n\
         caught true: {oob} 7\n\
         result 1\n\
         caught true: {oob} 9\n\
         result 5\n\
         caught true: {oob} 4\n\
         result 3\n\
         caught 100 of 100\n"
    );
    // Solo (one worker, no monitor: `joinPollSolo`), one worker with the monitor, four workers.
    for (threads, rescue) in [("1", None), ("1", Some("1")), ("4", None)] {
        let (code, out, err) = run_rescue(&exe, dir.path(), threads, rescue);
        assert_eq!(
            (code, out.as_str()),
            (0, want.as_str()),
            "VELT_THREADS={threads} VELT_RESCUE={rescue:?}, stderr: {err}"
        );
        // Each panic is printed once, where it happened.
        assert_eq!(
            err.matches("panic: ").count(),
            104,
            "VELT_THREADS={threads} VELT_RESCUE={rescue:?}, stderr: {err}"
        );
    }
}

#[test]
fn a_panicked_promise_nobody_awaits_is_dropped_and_one_outside_a_catch_ends_the_process() {
    if !catches_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("dropped.velt");
    std::fs::write(
        &file,
        r#"import { catchPanic } from "velt/task";

async function bad(): Promise<string> {
  await yieldNow();
  panic("nobody awaits this");
  return "never";
}

let kept: Promise<string>[] = [];

async function body(): Promise<string> {
  const ignored = bad();
  kept.push(bad());
  await yieldNow();
  await yieldNow();
  return "body done";
}

async function main() {
  console.log(await catchPanic(() => body()));
  console.log("still running");
  // A panicked promise started under the catch, awaited outside any: the process ends with 101,
  // as an uncaught panic, without printing the message a second time.
  const p = kept.pop()!;
  console.log(await p);
  console.log("not printed");
}
"#,
    )
    .unwrap();
    let exe = exe_name(dir.path(), "dropped");
    build_std(&file, &exe);
    let (code, out, err) = run(&exe, dir.path(), "1");
    assert_eq!(
        (code, out.as_str()),
        (101, "body done\nstill running\n"),
        "stderr: {err}"
    );
    assert_eq!(
        err.matches("panic: nobody awaits this").count(),
        2,
        "stderr: {err}"
    );
}

#[test]
fn only_a_program_that_catches_has_the_catch() {
    if !catches_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let body = r#"
async function at(i: i64): Promise<i64> {
  const xs: i64[] = [10, 20];
  await yieldNow();
  return xs[i]!;
}
"#;
    let plain = dir.path().join("plain.velt");
    std::fs::write(
        &plain,
        format!("{body}async function main() {{\n  const p = at(1);\n  console.log(await p, await spawn(at(0)));\n}}\n"),
    )
    .unwrap();
    let catching = dir.path().join("catching.velt");
    std::fs::write(
        &catching,
        format!("import {{ catchPanic }} from \"velt/task\";\n{body}async function main() {{\n  console.log(await catchPanic(() => at(1)));\n}}\n"),
    )
    .unwrap();
    let (plain_exe, catching_exe) = (
        exe_name(dir.path(), "plain"),
        exe_name(dir.path(), "catching"),
    );
    build_std(&plain, &plain_exe);
    build_std(&catching, &catching_exe);
    assert_eq!(run(&plain_exe, dir.path(), "1").1, "20 10\n");
    assert_eq!(run(&catching_exe, dir.path(), "1").1, "20\n");
    let has = |exe: &Path, sym: &[u8]| {
        std::fs::read(exe)
            .unwrap()
            .windows(sym.len())
            .any(|w| w == sym)
    };
    assert!(has(&catching_exe, b"velt_eh_try"));
    assert!(
        !has(&plain_exe, b"velt_eh_try"),
        "a program that never catches has no catch"
    );
    assert!(!has(&plain_exe, b"_Unwind_RaiseException"));
}
