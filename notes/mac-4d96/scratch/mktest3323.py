"""Writes crates/veltc/tests/std_scheduler_parked_away.rs from the #3323 probes.
python3 mktest3323.py <worktree>"""
import sys

wt = sys.argv[1]
P = "/Users/andii/dev/velt/branches/sleep-combo-var/p3323/"


def prog(name):
    s = open(P + name + ".velt").read()
    assert '"#' not in s
    return s


head = r'''//! A promise of a task handed to another task and dropped or freed there, on another worker,
//! while the task that started it is parked for ever (#3323, std/lang/hooks/sched_node_lost.velt
//! `droppedAway`): the drop gives its reference to the task and wakes it, and the task's own poll
//! gives the promise up (or frees it, finished) and then finds itself parked for ever, as #3274
//! (`sched_task_lost.velt`) frees a task whose promises it drives itself. Without that, the
//! promise and its task stayed to the end of the program. Each program runs counted with the
//! checking allocator (`VELT_RC_STATS=1`, `VELT_RT_DEBUG_ALLOC=1`) at 4 and 8 workers: exit 0, its
//! output, and every block freed. The programs pin tasks to workers 1 to 3, so not at one worker.

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
    assert!(o.status.success(), "{name}: {}", String::from_utf8_lossy(&o.stderr));
    exe
}

/// Every block allocated was freed (`VELT_RC_STATS`'s `blocks=allocated/freed`).
fn all_freed(err: &str) -> bool {
    err.split_whitespace()
        .find_map(|w| w.strip_prefix("blocks="))
        .and_then(|b| b.split_once('/'))
        .is_some_and(|(a, f)| a == f)
}

/// Builds `src` and runs it `runs` times at 4 and at 8 workers, counted: exit 0, `want` printed,
/// every block freed.
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
                .env_remove("VELT_RESCUE")
                .env_remove("VELT_SCHEDULER")
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n");
            let err = String::from_utf8_lossy(&o.stderr);
            if o.status.code() != Some(0) || out != want || !all_freed(&err) {
                bad.push(format!(
                    "{threads} workers, run {i}: exit {:?}, stdout {out:?}, stderr {err}",
                    o.status.code()
                ));
            }
        }
    }
    assert!(bad.is_empty(), "{name}: {} runs went wrong:\n{}", bad.len(), bad.join("\n"));
}

/// The issue's program: a task on worker 1 starts a promise that never settles and returns it
/// (in an object) to `main`, which hands it to a task on worker 2 that drops it unawaited. The
/// drop used to give back only the owner's reference, on worker 2, and the promise and the task
/// on worker 1 stayed: 9 blocks left.
#[test]
fn a_suspended_promise_dropped_on_another_worker_frees_its_task() {
    check("handed_drop", HANDED_DROP, "dropped on worker 2: 1\n", 3);
}

/// A promise handed on finishes and is awaited and freed on the task it was handed to, while
/// the task that started it is parked for ever. Its last reference may go there (the owner's
/// drop) or on the worker that woke it (the waker's own reference, released after the wake):
/// either way the promise's reference to its task went on a thread that is not the task's, and
/// nothing polled the task to see that it was now parked for ever (9 blocks a round, 180 in
/// all).
#[test]
fn a_finished_promise_freed_on_another_worker_lets_its_parked_task_go() {
    check("finished_away", FINISHED_AWAY, "the user got 42 in 20 rounds\n", 3);
}

/// Two promises of one task dropped at the same moment on two other workers: one hand-over wakes
/// the task while the other still holds its pin on it, so the task may give both up and end in
/// between; the pin keeps it until the second gives it back (5,100 blocks left before, over 300
/// rounds).
#[test]
fn two_promises_dropped_at_once_on_two_workers_free_their_task_once() {
    check("two_droppers", TWO_DROPPERS, "done 300 rounds, 600 drops\n", 3);
}

/// A promise that finishes after one turn, dropped on worker 2 at about the moment it finishes,
/// its task then parked for ever: the drop may read the promise's flags before the finish and
/// its count after it (once the task gave its reference back), and must not take a count of 1
/// for a promise its task still holds. Before, 1,500 blocks or more were left every run; with
/// the hand-over decided at the owner's drop only, 1 run in 8 still leaked under load.
#[test]
fn a_promise_dropped_as_it_finishes_lets_its_parked_task_go() {
    check("race_finish", RACE_FINISH, "done 200\n", 3);
}

/// Hand-overs racing the starting task's own polls: 400 tasks each hand a promise (one that never
/// settles, or one that finishes after a few turns) to a task on worker 2 or 3 that drops or
/// awaits it, poll on a while, and then end or park for ever (about 2,200 blocks left before).
#[test]
fn hand_overs_racing_their_tasks_polls_leave_nothing() {
    check(
        "mixed",
        MIXED,
        "266 ended, 400 handed on, 100 awaited\n",
        3,
    );
}
'''

out = head
for const, test, name in (
    ("HANDED_DROP", "a_suspended_promise_dropped_on_another_worker_frees_its_task", "handed_drop"),
    ("FINISHED_AWAY", "a_finished_promise_freed_on_another_worker_lets_its_parked_task_go", "t_finished"),
    ("TWO_DROPPERS", "two_promises_dropped_at_once_on_two_workers_free_their_task_once", "t_two"),
    ("RACE_FINISH", "a_promise_dropped_as_it_finishes_lets_its_parked_task_go", "t_race"),
    ("MIXED", "hand_overs_racing_their_tasks_polls_leave_nothing", "t_mixed"),
):
    out += f'\n/// The program of [`{test}`].\nconst {const}: &str = r#"{prog(name)}"#;\n'
open(wt + "/crates/veltc/tests/std_scheduler_parked_away.rs", "w").write(out)
