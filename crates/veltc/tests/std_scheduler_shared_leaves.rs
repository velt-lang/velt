//! Shared promises that aren't async calls (a `sleep`, a race, a spawned task's handle) under the
//! executor in Velt (std/lang/hooks/sched_share_leaf.velt, #2196): the leaf's address stays their
//! identity, and the leaf stays allocated while any holder lives; a shared handle whose task
//! panics under `catchPanic` stays shared. Each program is built with `VELT_SCHEDULER=std` and
//! run with 1, 4 and 8 workers.

use std::path::{Path, PathBuf};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// `src` built with `VELT_SCHEDULER=std` into `dir`: the executable.
fn build(dir: &Path, name: &str, src: &str) -> PathBuf {
    let file = dir.join(format!("{name}.velt"));
    std::fs::write(&file, src).unwrap();
    let exe = dir.join(format!("{name}{}", if cfg!(windows) { ".exe" } else { "" }));
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
    exe
}

/// Runs `exe` with `threads` workers and the block counters (with the debug runtime's checking
/// allocator when `checked`): it must exit with 0 and print `want`; with the debug runtime its
/// block counts must balance.
fn runs_even(exe: &Path, name: &str, want: &str, threads: &str, checked: bool) {
    let mut c = command(exe);
    c.env("VELT_THREADS", threads)
        .env("VELT_RC_STATS", "1")
        .env_remove("VELT_SCHEDULER");
    if checked {
        c.env("VELT_RT_DEBUG_ALLOC", "1");
    } else {
        c.env_remove("VELT_RT_DEBUG_ALLOC");
    }
    let o = c.output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    let at = format!("{name}, {threads} worker(s), checked {checked}");
    assert!(o.status.success(), "{at}: {err}");
    assert_eq!(
        String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n"),
        want,
        "{at}"
    );
    let Some(stats) = err
        .lines()
        .rev()
        .find(|l| l.starts_with("rc stats: retain"))
    else {
        // Only the debug runtime counts (a `--release` test run links the release one).
        if cfg!(debug_assertions) {
            panic!("{at}: no rc stats in:\n{err}");
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

/// A copy kept after the original was dropped and the promise settled is never `===` to a promise
/// made later: run without the checking allocator, whose quarantine never hands an address out
/// again, so a freed leaf's address is taken by the next promises (8 sleeps and 27 handles of 300
/// were before the hub kept the leaf).
#[test]
fn a_shared_leaf_keeps_its_identity_while_a_copy_lives() {
    if !cfg!(unix) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let name = "promise_identity_leaf_reuse";
    let exe = build(
        dir.path(),
        name,
        include_str!("../../../tests/golden/std/promise_identity_leaf_reuse.velt"),
    );
    let want = include_str!("../../../tests/golden/std/promise_identity_leaf_reuse.out");
    for threads in ["1", "4", "8"] {
        runs_even(&exe, name, want, threads, false);
        runs_even(&exe, name, want, threads, true);
    }
}

/// Several shared leaves alive at once, with counted results: each holder gets its own promise's
/// value (the table finds each leaf's record by its address), and every value is freed once.
#[test]
fn shared_leaves_alive_at_once_deliver_their_own_results() {
    if !cfg!(unix) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let name = "promise_identity_leaf_results";
    let exe = build(
        dir.path(),
        name,
        include_str!("../../../tests/golden/std/promise_identity_leaf_results.velt"),
    );
    let want = include_str!("../../../tests/golden/std/promise_identity_leaf_results.out");
    for threads in ["1", "4", "8"] {
        runs_even(&exe, name, want, threads, true);
    }
}

/// A shared handle whose task panics under `catchPanic` on another worker: a copy awaited
/// meanwhile gets the panic raised again, the handle is shared twice more after it, and an
/// original awaited after its copy's panic gets it raised too. The panic gives the shared
/// handle's record its new poll, not the handle's header: the driver polled the old one again
/// (ending the process as an uncaught panic), and the next share shared the handle a second time.
const PANICKED: &str = r#"import { catchPanic, workerCount } from "velt/task";
async function bad(): Promise<i64> {
  await yieldNow();
  panic("task boom");
  return 1;
}
async function awaitIt(p: Promise<i64>): Promise<i64> {
  return await p;
}
function message(e: unknown): string {
  return (e as Error).message.split(" at ")[0];
}
async function reshared(): Promise<string> {
  const h = spawn(bad(), { worker: workerCount() - 1 });
  const keep: Promise<i64>[] = [h];
  let caught = "";
  try {
    await catchPanic(() => awaitIt(keep[0]));
  } catch (e) {
    caught = message(e);
  }
  keep.push(h);
  keep.push(h);
  return `copies ${keep.length} ${keep[0] === keep[2]} ${caught}`;
}
async function awaited(): Promise<string> {
  const h = spawn(bad(), { worker: workerCount() - 1 });
  const keep: Promise<i64>[] = [h];
  let caught = "";
  try {
    await catchPanic(() => awaitIt(keep[0]));
  } catch (e) {
    caught = message(e);
  }
  return `awaited ${caught} ${await h} ${keep.length}`;
}
async function main() {
  console.log(await catchPanic(() => reshared()));
  try {
    console.log(await catchPanic(() => awaited()));
  } catch (e) {
    console.log("caught", message(e));
  }
  console.log("end");
}
"#;

#[test]
fn a_shared_handle_whose_task_panics_stays_shared_once() {
    // Generated code catches with the Itanium unwinder (std_scheduler_catch.rs `catches_here`).
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "panicked", PANICKED);
    for threads in ["1", "4", "8"] {
        runs_even(
            &exe,
            "panicked",
            "copies 3 true task boom\ncaught task boom\nend\n",
            threads,
            true,
        );
    }
}
