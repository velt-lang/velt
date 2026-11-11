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

/// Where the executor in Velt catches: generated code catches with the Itanium unwinder (macOS
/// and Linux; Windows' structured exceptions are not built yet, so a panic under `catchPanic`
/// ends a `VELT_SCHEDULER=std` program there: decision 3 on #2649).
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

/// Programs whose panicked promise or task is reached through the runtime's shared-promise hub
/// (velt_rt task/shared.rs: a promise read where it stays, `kept[0]`, or awaited at two places),
/// and what the default executor prints for each: its exit code and stdout.
const SHARED: &[(&str, &str, i32, &str)] = &[
    (
        // A task that panicked under a catch, joined from an array outside any: the hub kept the
        // join's poll from before the panic. It spun at 100% CPU; now it ends as uncaught.
        "shared_join_outside",
        r#"import { catchPanic, workerCount } from "velt/task";
async function bad(): Promise<i64> { await yieldNow(); panic("task boom"); return 1; }
let hs: Promise<i64>[] = [];
async function body(): Promise<string> { hs.push(spawn(bad(), { worker: workerCount() - 1 })); return "body"; }
async function main() {
  console.log(await catchPanic(() => body()));
  console.log("joining outside");
  console.log(await hs[0]);
  console.log("not printed");
}
"#,
        101,
        "body\njoining outside\n",
    ),
    (
        // A panicked promise awaited again in another catch: the raise unwound through the hub,
        // which kept the unwound waiter as its driver, so the next await waited for ever and the
        // program ended with 0 after "1 boom".
        "shared_awaited_again",
        r#"import { catchPanic } from "velt/task";
async function bad(): Promise<i64> { await yieldNow(); panic("boom"); return 0; }
let kept: Promise<i64>[] = [];
async function body(): Promise<string> { const b = bad(); kept.push(b); const r = await b; return "body"; }
async function again(): Promise<i64> { return await kept[0]; }
async function main() {
  try { console.log(await catchPanic(() => body())); } catch (e) { console.log("1", (e as Error).message.split(" at ")[0]); }
  try { console.log(await catchPanic(() => again())); } catch (e) { console.log("2", (e as Error).message.split(" at ")[0]); }
  console.log("end");
}
"#,
        0,
        "1 boom\n2 boom\nend\n",
    ),
    (
        // The same through `Promise.all` over a promise also kept in an array.
        "shared_all_again",
        r#"import { catchPanic } from "velt/task";
async function bad(): Promise<i64> { await yieldNow(); panic("boom"); return 0; }
async function good(): Promise<i64> { await yieldNow(); return 1; }
let kept: Promise<i64>[] = [];
async function body(): Promise<string> {
  const a = good();
  const b = bad();
  kept.push(b);
  const all = await Promise.all([a, b]);
  return `body ${all.length}`;
}
async function again(): Promise<i64> { return await kept[0]; }
async function main() {
  try { console.log(await catchPanic(() => body())); } catch (e) { console.log("1", (e as Error).message.split(" at ")[0]); }
  try { console.log(await catchPanic(() => again())); } catch (e) { console.log("2", (e as Error).message.split(" at ")[0]); }
  console.log("end");
}
"#,
        0,
        "1 boom\n2 boom\nend\n",
    ),
];

#[test]
fn a_panicked_promise_or_task_through_a_shared_promise_ends_or_raises_as_with_the_runtime() {
    if !catches_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for (name, src, want_code, want_out) in SHARED {
        let file = dir.path().join(format!("{name}.velt"));
        std::fs::write(&file, src).unwrap();
        let exe = exe_name(dir.path(), name);
        build_std(&file, &exe);
        for threads in ["1", "4"] {
            // A hang guard: the busy hang this tests for would otherwise hold the test for ever.
            let mut c = command(&exe);
            c.current_dir(dir.path())
                .env("VELT_THREADS", threads)
                .env_remove("VELT_SCHEDULER")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let child = c.spawn().unwrap();
            let id = child.id();
            let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let flag = done.clone();
            let guard = std::thread::spawn(move || {
                let start = std::time::Instant::now();
                while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                    // timing: a hang guard, not an order: a run that ends in time is the result.
                    if start.elapsed() > std::time::Duration::from_secs(20) {
                        let _ = command("kill").arg("-9").arg(id.to_string()).status();
                        return;
                    }
                    // timing: polls for the end the guard above bounds.
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            });
            let o = child.wait_with_output().unwrap();
            done.store(true, std::sync::atomic::Ordering::Relaxed);
            guard.join().unwrap();
            let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
            let err = String::from_utf8_lossy(&o.stderr);
            assert_eq!(
                (o.status.code().unwrap_or(-1), out.as_str()),
                (*want_code, *want_out),
                "{name} at {threads} workers; stderr: {err}"
            );
        }
    }
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

/// A guarded promise that finished before `catchPanic` waits (a handler that never waits) takes
/// the short path: its result straight away, no `settled` future (catch_sched.velt `take`); one
/// that panicked then, or waits, takes the long one. The catching polls nest (a `catchPanic` in
/// a promise polled through the catch) and share the worker's argument words, each one reading
/// its arguments before the nested one writes them and writing its result after.
#[test]
fn a_guarded_promise_done_before_the_wait_and_nested_catching_polls_give_their_results() {
    if !catches_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("short.velt");
    std::fs::write(
        &file,
        r#"import { catchPanic, PanicError } from "velt/task";

async function ok(i: i64): Promise<i64> {
  return i * 2;
}

async function bad(i: i64): Promise<i64> {
  if (i >= 0) {
    panic(`bad ${i}`);
  }
  return 0;
}

async function later(i: i64): Promise<i64> {
  await yieldNow();
  return i + 1;
}

async function nested(i: i64): Promise<i64> {
  const a = await catchPanic(() => ok(i));
  let b: i64 = 0;
  try {
    b = await catchPanic(() => bad(i));
  } catch (e) {
    b = 100;
  }
  const c = await catchPanic(() => later(i));
  await yieldNow();
  return a + b + c;
}

async function main() {
  let s: i64 = 0;
  for (let i: i64 = 0; i < 200; i++) {
    s += await catchPanic(() => ok(i));
    try {
      await catchPanic(() => bad(i));
    } catch (e) {
      if (e instanceof PanicError) {
        s += 1;
      }
    }
    s += await catchPanic(() => later(i));
    s += await catchPanic(() => nested(i));
  }
  console.log(s);
}
"#,
    )
    .unwrap();
    let exe = exe_name(dir.path(), "short");
    build_std(&file, &exe);
    // Per i: 2i + 1 + (i + 1) + (2i + 100 + i + 1) = 6i + 103.
    for threads in ["1", "4"] {
        let (code, out, err) = run(&exe, dir.path(), threads);
        assert_eq!(
            (code, out.as_str()),
            (0, "140000\n"),
            "{threads} workers; stderr: {err}"
        );
        assert_eq!(err.matches("panic: bad ").count(), 400, "stderr: {err}");
    }
}

/// A promise started inside a catching poll (http-server's handler, started by its adapter that
/// `catchPanic` guards) whose first, synchronous part panics, awaited at once by the code that
/// started it: the panic reaches `catchPanic` through that await, and a task that caught many such
/// panics still finishes and its join returns. A guarded promise still running when `catchPanic`
/// waits for it is waited for through the settled future inside its node (catch_sched.velt
/// `settled`): a panic after the wait began is a `PanicError`, a result is delivered whole.
#[test]
fn a_panic_in_the_first_run_of_a_promise_started_under_a_catch_and_one_after_the_wait_are_caught() {
    if !catches_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("first.velt");
    std::fs::write(
        &file,
        r#"import { catchPanic, PanicError } from "velt/task";

function sub(i: i64): Promise<i64> {
  return subAsync(i);
}

async function subAsync(i: i64): Promise<i64> {
  if (i % 5 == 0) {
    panic(`sub ${i}`);
  }
  return i;
}

async function handler(i: i64): Promise<i64> {
  if (i % 3 == 0) {
    panic(`handler ${i}`);
  }
  // Started in the handler's own first run: two promises in their first run when it panics.
  return await sub(i);
}

async function adapter(i: i64): Promise<i64> {
  const r = await handler(i);
  // The adapter waits a turn, as http-server's does: `catchPanic` waits for it.
  await yieldNow();
  return r + 1;
}

async function laterBad(i: i64): Promise<i64> {
  await yieldNow();
  if (i % 2 == 0) {
    panic(`later ${i}`);
  }
  return i;
}

async function laterStr(i: i64): Promise<string> {
  await yieldNow();
  return `v${i}`;
}

async function work(n: i64): Promise<i64> {
  let s: i64 = 0;
  for (let i: i64 = 0; i < n; i++) {
    try {
      s += await catchPanic(() => adapter(i));
    } catch (e) {
      if (e instanceof PanicError) {
        s += e.message.startsWith("handler") ? 1000 : 2000;
      }
    }
    try {
      s += await catchPanic(() => laterBad(i));
    } catch (e) {
      if (e instanceof PanicError) {
        s += 1;
      }
    }
    const v = await catchPanic(() => laterStr(i));
    s += v.length as i64;
  }
  await yieldNow();
  return s;
}

async function main() {
  console.log(await spawn(work(300)));
  console.log(await work(30));
}
"#,
    )
    .unwrap();
    let exe = exe_name(dir.path(), "first");
    build_std(&file, &exe);
    for threads in ["1", "4"] {
        let (code, out, err) = run(&exe, dir.path(), threads);
        assert_eq!(
            (code, out.as_str()),
            (0, "227900\n18576\n"),
            "{threads} workers; stderr: {err}"
        );
        assert_eq!(
            err.matches("panic: handler ").count() + err.matches("panic: sub ").count(),
            154,
            "stderr: {err}"
        );
        assert_eq!(err.matches("panic: later ").count(), 165, "stderr: {err}");
    }
}

/// A promise started while a catching poll runs keeps a catch of its own for its first,
/// synchronous run, as with the default executor: when that run panics, the promise alone is
/// panicked, and the code that started it goes on. A starter that never awaits it returns
/// normally (its result reaches `catchPanic`), and one that awaits it later runs up to that await
/// first, then gets the panic there. Built with both executors, the outputs are compared.
const STARTED_UNDER_A_CATCH: &str = r#"import { catchPanic, PanicError } from "velt/task";

async function bad(i: i64): Promise<i64> {
  if (i % 2 == 0) {
    panic(`first ${i}`);
  }
  return i;
}

function start(i: i64): Promise<i64> {
  return bad(i);
}

async function noAwait(i: i64): Promise<i64> {
  const p = start(i);
  return i + 100;
}

async function awaitLater(i: i64, seen: i64[]): Promise<i64> {
  const p = start(i);
  seen.push(i);
  await yieldNow();
  return await p;
}

async function work(n: i64): Promise<string> {
  let s: i64 = 0;
  let panics: i64 = 0;
  const seen: i64[] = [];
  for (let i: i64 = 0; i < n; i++) {
    try {
      s += await catchPanic(() => noAwait(i));
    } catch (e) {
      panics += 1;
    }
    try {
      s += await catchPanic(() => awaitLater(i, seen));
    } catch (e) {
      if (e instanceof PanicError) {
        panics += 1000;
      }
    }
  }
  return `${s} ${panics} ${seen.length}`;
}

async function main() {
  console.log(await spawn(work(20)));
  console.log(await work(10));
}
"#;

#[test]
fn a_promise_started_under_a_catch_panics_alone_as_with_the_default_executor() {
    if !catches_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("started.velt");
    std::fs::write(&file, STARTED_UNDER_A_CATCH).unwrap();
    let std_exe = exe_name(dir.path(), "started_std");
    build_std(&file, &std_exe);
    let default_exe = exe_name(dir.path(), "started_default");
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .arg("build")
        .arg(&file)
        .arg("-o")
        .arg(&default_exe)
        .current_dir(dir.path())
        .env_remove("VELT_SCHEDULER")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // Each noAwait returns i + 100; each odd awaitLater returns i; each even one panics.
    let want = "2290 10000 20\n1070 5000 10\n";
    for threads in ["1", "4"] {
        let (code, out, err) = run(&default_exe, dir.path(), threads);
        assert_eq!(
            (code, out.as_str()),
            (0, want),
            "default executor, {threads} workers; stderr: {err}"
        );
        let (code, out, err) = run(&std_exe, dir.path(), threads);
        assert_eq!(
            (code, out.as_str()),
            (0, want),
            "VELT_SCHEDULER=std, {threads} workers; stderr: {err}"
        );
        assert_eq!(err.matches("panic: first ").count(), 30, "stderr: {err}");
    }
}

/// A guarded promise that waits for another task (as a handler waits for I/O) is not adopted by
/// `catchPanic`'s wait while it drives itself (catch_sched.velt `settledPoll`, sched_node_catch
/// `drivesItself`): its own wake runs it, and its finish, with a result or a panic after the
/// wait, wakes the wait. The peer task answers on another worker at 4 workers.
#[test]
fn a_guarded_promise_waiting_for_another_task_is_run_by_its_own_wake() {
    if !catches_here() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join("peer.velt");
    std::fs::write(
        &file,
        r#"import { catchPanic, PanicError } from "velt/task";
import { Channel, channel } from "velt/sync";

async function handler(reqs: Channel<i64>, reps: Channel<i64>, i: i64): Promise<i64> {
  await reqs.send(i);
  const r = (await reps.receive()) ?? 0;
  if (i % 3 == 0) {
    panic(`late ${i}`);
  }
  return r * 2;
}

async function adapter(reqs: Channel<i64>, reps: Channel<i64>, i: i64): Promise<i64> {
  const r = await handler(reqs, reps, i);
  return r + 1;
}

async function main() {
  const reqs = channel<i64>();
  const reps = channel<i64>();
  const n: i64 = 300;
  const peer = spawn(async () => {
    for (let k: i64 = 0; k < n; k++) {
      const x = await reqs.receive();
      await reps.send(x ?? 0);
    }
  });
  let s: i64 = 0;
  for (let i: i64 = 0; i < n; i++) {
    try {
      s += await catchPanic(() => adapter(reqs, reps, i));
    } catch (e) {
      if (e instanceof PanicError) {
        s += 1000;
      }
    }
  }
  await peer;
  console.log(s);
}
"#,
    )
    .unwrap();
    let exe = exe_name(dir.path(), "peer");
    build_std(&file, &exe);
    // 100 panics (i a multiple of 3) give 1000 each; the other i give 2i + 1, 60,200 in all.
    for threads in ["1", "4"] {
        let (code, out, err) = run(&exe, dir.path(), threads);
        assert_eq!(
            (code, out.as_str()),
            (0, "160200\n"),
            "{threads} workers; stderr: {err}"
        );
        assert_eq!(err.matches("panic: late ").count(), 100, "stderr: {err}");
    }
}
