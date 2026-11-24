"""Writes crates/veltc/tests/std_scheduler_wake_order.rs from the probe programs (#3487)."""
import sys

W = "$HOME/dev/velt/branches/fix-3420-254e"
I = "$HOME/dev/velt/branches/r3420-254e/i3487"

HEAD = '''//! A wake by value from a thread that is not the task's home worker hands the waker's reference
//! to the task's next poll, which gives it back before any park counts (#3487,
//! std/lang/hooks/sched_wake_away.velt). Before, the woken task could run and park on its own
//! worker between the wake and the release, its park counted the waker's reference, and the
//! release that followed gave it back with no poll: a task parked for ever stayed to the end of
//! the program. That window opens only when the waking thread loses its core right after the wake
//! (a busy machine); `VELT_SCHED_WAKE_PAUSE`, read with the block counters on, holds it open after
//! every wake from another thread, so each run goes through it. Each program runs counted with the
//! checking allocator at 4 and 8 workers: exit 0, its output, and every block freed.

use std::path::{Path, PathBuf};

use no_window::command;

mod no_window;
mod runtime_support;
mod test_dir;

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Workers only start where `#sched-thread` has threads (macOS, Linux and Windows).
fn has_threads() -> bool {
    cfg!(any(target_os = "macos", target_os = "linux", windows))
}

/// `src` built with `VELT_SCHEDULER=std` in `dir`: the executable.
fn build_std(dir: &Path, name: &str, src: &str) -> PathBuf {
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
    exe
}

/// Every block allocated was freed (`VELT_RC_STATS`'s `blocks=allocated/freed`).
fn all_freed(err: &str) -> bool {
    err.split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))
        .and_then(|b| b.split_once('/'))
        .is_some_and(|(a, f)| a == f)
}

/// Builds `src` and runs it `runs` times at 4 and at 8 workers, counted, with the window after a
/// wake from another thread held open (2,000 turns of the core): exit 0, `want` printed, every
/// block freed.
fn check(name: &str, src: &str, want: &str, runs: usize) {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), name, src);
    let mut bad = Vec::new();
    for threads in ["4", "8"] {
        for i in 0..runs {
            let o = command(&exe)
                .env("VELT_THREADS", threads)
                .env("VELT_RT_DEBUG_ALLOC", "1")
                .env("VELT_RC_STATS", "1")
                .env("VELT_SCHED_WAKE_PAUSE", "2000")
                .env_remove("VELT_RESCUE")
                .env_remove("VELT_SCHEDULER")
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&o.stdout).replace("\\r\\n", "\\n");
            let err = String::from_utf8_lossy(&o.stderr);
            if o.status.code() != Some(0) || out != want || !all_freed(&err) {
                bad.push(format!(
                    "{threads} workers, run {i}: exit {:?}, stdout {out:?}, stderr {err}",
                    o.status.code()
                ));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{name}: {} runs went wrong:\\n{}",
        bad.len(),
        bad.join("\\n")
    );
}

/// A task's root waker woken by value on worker 2 (the #3485 review's `awaited_dropped`): the
/// task then parks for ever on a holder kept in module state. With the old order every run left
/// blocks (on a busy Linux machine 36 to 54 runs of 100 did, without the pause).
#[test]
fn a_task_woken_from_another_worker_is_found_parked_for_ever() {
    check("awaited_dropped", AWAITED_DROPPED, "dropped 30\\n", 3);
}

/// A promise's node waker woken by value on worker 3, the promise not adopted by its owner: it
/// then waits for ever. With the old order every run left blocks.
#[test]
fn a_promise_woken_from_another_worker_is_found_owner_only() {
    check("node_woken_away", NODE_WOKEN_AWAY, "done 20\\n", 3);
}

/// Tasks woken from worker 3 while their home worker runs synchronous code, so another worker may
/// take them from its queue: the reference each wake hands the task's next poll goes back once,
/// wherever that poll runs (sched_wake_away.velt `polled`). Giving it back twice, or not at all,
/// left blocks in most runs.
#[test]
fn a_woken_task_taken_by_another_worker_gives_the_reference_back_once() {
    check("stolen_carry", STOLEN_CARRY, "parked 40 true\\n", 5);
}

/// `main` ends while a task woken from another worker is still queued behind a busy one on its
/// home worker: the program still runs it, and its poll gives the carried reference back.
#[test]
fn a_carried_wake_still_queued_when_main_ends_is_given_back() {
    check("exit_queued", EXIT_QUEUED, "main ends\\nbusy true\\n", 3);
}
'''

out = [HEAD]
for name, fn, f in [
    ("AWAITED_DROPPED", "a_task_woken_from_another_worker_is_found_parked_for_ever", "awaited_dropped.velt"),
    ("NODE_WOKEN_AWAY", "a_promise_woken_from_another_worker_is_found_owner_only", "node_wake2.velt"),
    ("STOLEN_CARRY", "a_woken_task_taken_by_another_worker_gives_the_reference_back_once", "stolen_carry.velt"),
    ("EXIT_QUEUED", "a_carried_wake_still_queued_when_main_ends_is_given_back", "exit_queued.velt"),
]:
    prog = open(f"{I}/{f}").read()
    if '"#' in prog:
        sys.exit(f"{f}: contains a raw-string end")
    out.append(f"\n/// The program of [`{fn}`].\nconst {name}: &str = r#\"{prog}\"#;\n")
open(f"{W}/crates/veltc/tests/std_scheduler_wake_order.rs", "w").write("".join(out))
print("written")
