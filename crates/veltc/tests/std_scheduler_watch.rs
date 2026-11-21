//! The unhandled rejection of a held promise under the executor in Velt (#2854 step 4;
//! std/lang/hooks/sched_node_report.velt): a started promise that rejects while its owner holds it
//! without waiting goes on its task's unhandled list as it finishes, and the checkpoint that
//! drains the list after the microtasks reports it if it is still nobody's, as Node reports a
//! rejection without a handler once the microtask queue is drained. Each program is built with
//! the executor in Velt and runs with the debug runtime's block counters
//! (`VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1`) at one worker and at more.

use std::path::Path;

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `src` built with `VELT_SCHEDULER=std`, then run with `VELT_THREADS=1`: its stdout.
fn build_and_run(dir: &Path, name: &str, src: &str) -> String {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(exe_name(name));
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
    let o = command(&exe)
        .env("VELT_THREADS", "1")
        .env_remove("VELT_SCHEDULER")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
    assert!(
        o.status.success(),
        "{name}: {out}{}",
        String::from_utf8_lossy(&o.stderr)
    );
    out
}

fn exe_name(name: &str) -> String {
    format!("{name}{}", if cfg!(windows) { ".exe" } else { "" })
}

/// Builds and runs `src`, which prints `want`; then `runs` times with one worker and with four,
/// counted: it prints `want` and frees every block.
fn prints_counted(name: &str, src: &str, want: &str, runs: usize) {
    // A `VELT_SCHEDULER=std` program builds on unix only (no reactor in Velt on Windows yet).
    if !cfg!(unix) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    assert_eq!(build_and_run(dir.path(), name, src), want, "{name}");
    counted_runs(dir.path(), name, want, runs);
}

/// The std build of `name` in `dir` run `runs` times with one worker and with four, with the
/// debug runtime's block counters: it prints `want` and frees every block.
fn counted_runs(dir: &Path, name: &str, want: &str, runs: usize) {
    let exe = dir.join(exe_name(name));
    for threads in ["1", "4"] {
        for run in 0..runs {
            let o = command(&exe)
                .env("VELT_THREADS", threads)
                .env("VELT_RT_DEBUG_ALLOC", "1")
                .env("VELT_RC_STATS", "1")
                .env_remove("VELT_SCHEDULER")
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&o.stderr);
            let at = format!("{name}, {threads} worker(s), run {run}");
            assert!(o.status.success(), "{at}: {stderr}");
            assert_eq!(
                String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
                want,
                "{at}"
            );
            let Some(stats) = stderr.lines().rev().find(|l| l.starts_with("rc stats:")) else {
                // Only the debug runtime counts (a `--release` test run links the release one).
                if cfg!(debug_assertions) {
                    panic!("{at}: no rc stats in:\n{stderr}");
                }
                return;
            };
            let blocks = stats
                .split_whitespace()
                .find_map(|w| w.strip_prefix("blocks="))
                .and_then(|b| b.split_once('/'))
                .unwrap_or_else(|| panic!("{at}: no block counts in {stats}"));
            assert_eq!(blocks.0, blocks.1, "{at}: blocks left unfreed ({stats})");
        }
    }
}

/// A rejected promise its owner holds is watched until the microtasks drain (#2854 step 4):
/// awaited, handed to a combinator, given a `catch`, or handed to a task that handles it before
/// then, it is not reported. Node prints the same (with the task's `await` for `spawn`).
#[test]
fn a_held_rejection_handled_before_the_microtasks_drain_is_not_reported() {
    prints_counted(
        "held_rejections_handled",
        HELD_REJECTIONS,
        "caught a\nsettled rejected\ncatch c\ntask caught d\nend\n",
        2,
    );
}

/// Promises handed to tasks that drop them without awaiting, while their own task finishes
/// them on another worker: every result is dropped once, counted at four workers. (The drop and
/// the finish each look at the flags before their own or: exactly one sees the other. This
/// program did not catch the old order in 10 runs, so it guards the drop paths, not the race.)
#[test]
fn promises_dropped_on_another_worker_as_they_finish_free_their_results() {
    prints_counted(
        "dropped_as_finished",
        DROPPED_AS_FINISHED,
        "dropped 10000\n",
        3,
    );
}

/// The program of [`a_held_rejection_handled_before_the_microtasks_drain_is_not_reported`].
const HELD_REJECTIONS: &str = r#"async function fail(tag: string): Promise<string, Error> {
  throw new Error(tag);
}
async function take(p: Promise<string, Error>): Promise<string> {
  try {
    return await p;
  } catch (e) {
    return `task caught ${(e as Error).message}`;
  }
}
async function main() {
  // Awaited before the microtasks drain: handled.
  const a = fail("a");
  try {
    await a;
  } catch (e) {
    console.log("caught", (e as Error).message);
  }
  // Handed to a combinator before the checkpoint: handled.
  const b = fail("b");
  const s = await Promise.allSettled([b]);
  console.log("settled", s[0].status);
  // Its rejection handled by `catch` before the checkpoint.
  const c = fail("c");
  c.catch(() => console.log("catch c"));
  // Handed to another task, which handles it.
  const d = fail("d");
  console.log(await spawn(take(d)));
  await sleep(5);
  console.log("end");
}
"#;

/// The program of [`promises_dropped_on_another_worker_as_they_finish_free_their_results`].
const DROPPED_AS_FINISHED: &str = r#"class Box {
  n: i64 = 0;
}
// Finishes after a few microtask ticks on main's worker.
async function make(i: i64): Promise<Box> {
  for (let k = 0; k < (i % 4); k++) {
    await Promise.resolve();
  }
  const b = new Box();
  b.n = i;
  return b;
}
// Drops the promise it was handed without awaiting it, on whichever worker runs it.
async function drop(p: Promise<Box>): Promise<i64> {
  return 1;
}
async function main() {
  let dropped: i64 = 0;
  for (let round = 0; round < 200; round++) {
    const hs: Promise<i64>[] = [];
    for (let i = 0; i < 50; i++) {
      hs.push(spawn(drop(make(i))));
    }
    for (const h of hs) {
      dropped += await h;
    }
  }
  console.log("dropped", dropped);
}
"#;

/// `src` built, then run at each of `threads` with the debug runtime's block counters: it prints
/// `want` on stdout, reports exactly one uncaught error starting with `report` and exits with 1;
/// with `balanced`, it frees every block (otherwise what `main`'s suspended frame holds at the
/// exit stays counted: an exit on an uncaught error does not give back what a suspended frame
/// holds).
fn reports_once(name: &str, src: &str, want: &str, report: &str, threads: &[&str], balanced: bool) {
    if !cfg!(unix) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let file = dir.path().join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.path().join(exe_name(name));
    let o = command(env!("CARGO_BIN_EXE_velt"))
        .args([
            "build".as_ref(),
            file.as_os_str(),
            "-o".as_ref(),
            exe.as_os_str(),
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    for threads in threads {
        let o = command(&exe)
            .env("VELT_THREADS", threads)
            .env("VELT_RT_DEBUG_ALLOC", "1")
            .env("VELT_RC_STATS", "1")
            .output()
            .unwrap();
        let (out, err) = (
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr),
        );
        let at = format!("{name}, {threads} worker(s)");
        assert_eq!(out.replace("\r\n", "\n"), want, "{at}: {err}");
        let reports: Vec<&str> = err.lines().filter(|l| l.contains("Uncaught")).collect();
        assert_eq!(reports.len(), 1, "{at}: {err}");
        assert!(reports[0].starts_with(report), "{at}: {err}");
        assert_eq!(o.status.code(), Some(1), "{at}: {err}");
        if balanced && cfg!(debug_assertions) {
            let stats = err
                .lines()
                .rev()
                .find(|l| l.starts_with("rc stats:"))
                .unwrap_or("");
            let blocks = stats
                .split_whitespace()
                .find_map(|w| w.strip_prefix("blocks="))
                .and_then(|b| b.split_once('/'))
                .unwrap_or_else(|| panic!("{at}: no block counts in {err}"));
            assert_eq!(blocks.0, blocks.1, "{at}: blocks left unfreed ({stats})");
        }
    }
}

/// A watched rejection its owner drops on its own task before the checkpoint (`let p = fail();
/// p = ok();`) is reported once by the checkpoint, its result freed (sched_node_report.velt
/// `dropResult`'s WATCHED branch): without that branch the node went on the list twice.
#[test]
fn a_watched_rejection_its_owner_drops_is_reported_once() {
    let src = "async function fail(): Promise<number> {\n  throw new Error(\"boom\");\n}\nasync function ok(): Promise<number> {\n  return 1;\n}\nasync function main() {\n  let p = fail();\n  p = ok();\n  await null;\n  console.log(\"after\", await p);\n}\n";
    reports_once(
        "watched_dropped",
        src,
        "after 1\n",
        "Uncaught Error: boom at ",
        &["1", "4"],
        true,
    );
}

/// A watched converted promise its owner drops after someone handled the promise it stands for:
/// the checkpoint drops its result quietly (DETACHED), so nothing is reported and every block is
/// freed. Without DETACHED the checkpoint skipped it as handled, and its result leaked.
#[test]
fn a_watched_converted_rejection_handled_elsewhere_is_dropped_quietly() {
    let src = "async function fail(): Promise<number> {\n  await Promise.resolve();\n  throw new Error(\"a\");\n}\nasync function main() {\n  const p = fail();\n  let u: Promise<unknown> = p;\n  try {\n    await p;\n  } catch (e) {\n    console.log(\"caught\");\n  }\n  u = Promise.resolve(0);\n  await null;\n  console.log(\"done\", u !== null);\n}\n";
    prints_counted("watched_converted_handled", src, "caught\ndone true\n", 3);
}

/// A watched rejection handed to a task that drops it is that task's to report (as any
/// handed-over promise), at every worker count: main's checkpoint only gives back its reference.
/// Both sides used to decide it (the drop read WATCHED while the checkpoint cleared it), and the
/// report came before `joined` at one worker but after it at four.
#[test]
fn a_watched_rejection_handed_to_a_task_is_reported_there_once() {
    let src = "function busy(n: number): number {\n  let x = 0;\n  for (let k = 0; k < n; k++) {\n    x = (x * 31 + k) % 1000003;\n  }\n  return x;\n}\nasync function fail(i: number): Promise<number> {\n  throw new Error(\"x\" + i);\n}\nasync function drop(p: Promise<number>): Promise<number> {\n  return 1;\n}\nasync function main() {\n  const p = fail(0);\n  const t = spawn(drop(p));\n  busy(3000000);\n  await null;\n  console.log(\"joined\", await t);\n}\n";
    reports_once(
        "watched_handed",
        src,
        "",
        "Uncaught Error: x0 at ",
        &["1", "4", "8"],
        false,
    );
}

/// A converted promise's rejection reported at the checkpoint points at the `throw` of the promise
/// it stands for: the conversion's glue raises the error again with no location of its own.
#[test]
fn a_watched_converted_rejection_is_reported_where_it_was_thrown() {
    let src = "async function fail(): Promise<number> {\n  throw new Error(\"a\");\n}\nasync function main() {\n  const p = fail();\n  const u: Promise<unknown> = p;\n  await sleep(0);\n  try {\n    await u;\n  } catch (e) {\n    console.log(\"caught\");\n  }\n}\n";
    reports_once(
        "watched_converted_loc",
        src,
        "",
        "Uncaught Error: a at ",
        &["1", "4"],
        false,
    );
}
