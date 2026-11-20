"""Writes crates/veltc/tests/std_scheduler_sleep_handed.rs from the probe programs here."""
import os

here = os.path.dirname(os.path.abspath(__file__))


def prog(name):
    return open(os.path.join(here, name + ".velt")).read()


head = r'''//! A sleep handed to another task (#3207, std/lang/hooks/sched_sleep_move.velt): a promise that
//! holds a sleep is polled by whichever task awaits it now, and dropped by whoever holds it
//! last, on that task's worker, which need not be the one whose timer block holds the sleep.
//! Such a poll or drop goes through the holding worker. The runs are counted
//! (`VELT_RC_STATS=1`), which turns on std's check of the rule (a sleep is touched only on the
//! worker that holds it, and expires for a task of that worker: an `ICE:` otherwise), so a
//! program that breaks it fails every time, not only when memory happens to break.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use no_window::command;
use sched_workers_support::{has_threads, manifest};

mod no_window;
mod runtime_support;
mod sched_workers_support;
mod test_dir;

/// `src` built under the executor in Velt in `dir`: the executable.
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
    assert!(o.status.success(), "{name}: {}", String::from_utf8_lossy(&o.stderr));
    exe
}

/// Runs `exe` with `threads` workers, counted and with the checking allocator, killed after a
/// minute: its exit code (`None` when killed) and output.
fn run(exe: &Path, threads: &str) -> (Option<i32>, String) {
    let mut child = command(exe)
        .env_remove("VELT_SCHEDULER")
        .env_remove("VELT_RESCUE")
        .env("VELT_THREADS", threads)
        .env("VELT_RC_STATS", "1")
        .env("VELT_RT_DEBUG_ALLOC", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    let code = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status.code();
        }
        if start.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        // timing: polls the child for its exit under a one-minute cap, assumes no timing
        std::thread::sleep(Duration::from_millis(20));
    };
    let o = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    (code, text)
}

/// Whether the `rc stats:` line shows as many blocks freed as allocated.
fn balanced(out: &str) -> bool {
    out.lines()
        .find(|l| l.starts_with("rc stats:"))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("blocks=")))
        .and_then(|b| b.split_once('/'))
        .is_some_and(|(a, f)| a == f)
}

/// Runs `exe` `runs` times with `threads` workers: exit 0, output starting with `want`, blocks
/// balanced.
fn check(exe: &Path, threads: &str, runs: usize, want: &str) {
    for i in 0..runs {
        let (code, out) = run(exe, threads);
        assert_eq!(code, Some(0), "run {i} at {threads} workers: {out}");
        assert!(out.starts_with(want), "run {i} at {threads} workers: {out}");
        assert!(balanced(&out), "run {i} at {threads} workers, blocks: {out}");
    }
}

/// A task pinned to worker 1 polls a sleep (pending in worker 1's block from then on) and hands
/// its promise to a task pinned to worker 2, which awaits it: worker 1 gives the sleep up, and it
/// fires in worker 2's block. Without the hand-over the sleep expired on worker 1 for a task of
/// worker 2 (the check's `ICE:`, every run).
#[test]
fn a_sleep_awaited_on_another_worker_moves_to_it() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "pinned_poll", PINNED_POLL);
    check(&exe, "4", 10, "awaited on worker 2\n");
}

/// As above, but the task on worker 2 drops the promise unawaited: worker 1 takes the sleep off
/// its block and frees it, and the program ends at once instead of waiting ten minutes for it
/// (the run's limit is one). Without the hand-over worker 2 unlinked it from a list it was not
/// in (the check's `ICE:`, every run).
#[test]
fn a_sleep_dropped_on_another_worker_leaves_its_block() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "pinned_drop", PINNED_DROP);
    check(&exe, "4", 10, "dropped on worker 2\n");
}

/// A sleep pending in worker 1's block awaited by three tasks at once, two on worker 2 and one on
/// worker 0: each asks worker 1, while another's request may be on its way or just served (the
/// sleep then new again, in the asker's block, or still in worker 1's). Every task wakes once.
/// Without the hand-over the sleep expired on worker 1 for a task of another worker (the check's
/// `ICE:`, every run).
#[test]
fn a_sleep_awaited_from_three_tasks_on_other_workers_wakes_them_all() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "pinned_three", PINNED_THREE);
    check(&exe, "4", 10, "awaited on workers 2 2 0\n");
}

/// The shapes that crashed (#3201's review): tasks hand polled sleeps on while busy tasks make
/// worker 0 offer its queue, so the second poller is fresh on another worker, runs in its waker's
/// slot, or is offered between its polls; and two tasks await one sleep, each asking for it while
/// the other's request may be under way; and the same load with no hand-over at all, whose
/// offered tasks stayed in an offer no busy worker took (sched_spread.velt `reclaim`). Every
/// sleep fires once, at 4 and 8 workers.
#[test]
fn handed_on_sleeps_fire_under_offers() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    for (name, src) in [
        ("handed_user", HANDED_USER),
        ("handed_maker", HANDED_MAKER),
        ("handed_two", HANDED_TWO),
        ("unhanded", UNHANDED),
    ] {
        let exe = build(dir.path(), name, src);
        check(&exe, "4", 4, "fired 1000\n");
        check(&exe, "8", 4, "fired 1000\n");
    }
}

/// A handed sleep polled once more on the new task's worker (registered there, or still asked
/// for) and then dropped: the drop may be served on another worker than the request before it,
/// and whichever ends last frees the sleep, once (blocks balanced), at 4 and 8 workers.
#[test]
fn a_handed_sleep_dropped_after_its_request_is_freed_once() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "handed_drop", HANDED_DROP);
    check(&exe, "4", 4, "done 1000\n");
    check(&exe, "8", 4, "done 1000\n");
}
'''


def const(name, doc, text):
    return f'\n/// {doc}\nconst {name}: &str = r#"{text}"#;\n'


out = head
out += const("PINNED_POLL", "The program of [`a_sleep_awaited_on_another_worker_moves_to_it`].", prog("pinned_poll"))
out += const("PINNED_DROP", "The program of [`a_sleep_dropped_on_another_worker_leaves_its_block`].", prog("pinned_drop"))
out += const(
    "PINNED_THREE",
    "The program of [`a_sleep_awaited_from_three_tasks_on_other_workers_wakes_them_all`].",
    prog("pinned_three"),
)
out += const(
    "HANDED_USER",
    "[`handed_on_sleeps_fire_under_offers`]: a task that ended polled the sleep first; another awaits it.",
    prog("handed4"),
)
out += const(
    "HANDED_MAKER",
    "[`handed_on_sleeps_fire_under_offers`]: the task that polled the sleep first hands it on and yields on.",
    prog("handed5"),
)
out += const(
    "HANDED_TWO",
    "[`handed_on_sleeps_fire_under_offers`]: two tasks await one sleep the task that polled it first handed on.",
    prog("handed7"),
)
out += const(
    "UNHANDED",
    "[`handed_on_sleeps_fire_under_offers`]: `HANDED_USER`'s load with each sleep awaited by the task that polled it.",
    prog("handed4ctl"),
)
out += const(
    "HANDED_DROP",
    "The program of [`a_handed_sleep_dropped_after_its_request_is_freed_once`].",
    prog("handed9"),
)
open("/Users/andii/dev/velt/branches/sleep-home/crates/veltc/tests/std_scheduler_sleep_handed.rs", "w").write(out)
print(len(out.splitlines()), "lines")
