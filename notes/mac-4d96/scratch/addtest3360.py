"""Appends #3360's test to std_scheduler_handed_promises.rs: python3 addtest3360.py <worktree>"""
import sys

p = sys.argv[1] + "/crates/veltc/tests/std_scheduler_handed_promises.rs"
s = open(p).read()
prog = open("/Users/andii/dev/velt/branches/sleep-combo-var/p3323/settle_join.velt").read()
assert '"#' not in prog
s = s.rstrip("\n") + '''

/// Runs of [`SETTLED_ACROSS`] at each worker count.
const SETTLE_RUNS: usize = 5;

/// A task on worker 1 settles `main`'s promise with an object that holds a promise of its own,
/// and `main` hands the object on to a third task the moment it is settled. The settling step
/// (std/lang/promise/settle.velt `fulfillLater`) used to open the promise's latch while its
/// frame still referenced the object: handed on in that window the object was copied, the copy
/// shared the promise inside on `main`'s task, and the frame's own reference was then dropped
/// on worker 1, a holder of a shared promise dropped off its task (an `ICE:`, #3360). The window
/// is a few instructions, so `main`'s std passes this nearly always on a quiet machine (1 run
/// in 30 failed on a loaded one); with the window widened (the settling step spinning 0.3 ms
/// before its frame ends) `main`'s std fails every run and the fix none.
#[test]
fn a_value_settled_from_another_task_is_let_go_before_its_awaiter_sees_it() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build_std(dir.path(), "settled_across", SETTLED_ACROSS);
    let mut bad = Vec::new();
    for threads in ["4", "8"] {
        for i in 0..SETTLE_RUNS {
            let (code, out, err) = run(&exe, threads);
            if code != 0 || out != "done 600\\n" || !all_freed(&err) {
                bad.push(format!(
                    "{threads} workers, run {i}: exit {code}, stdout {out:?}, stderr {err}"
                ));
            }
        }
    }
    assert!(bad.is_empty(), "{} runs went wrong:\\n{}", bad.len(), bad.join("\\n"));
}

/// The program of [`a_value_settled_from_another_task_is_let_go_before_its_awaiter_sees_it`].
const SETTLED_ACROSS: &str = r#"''' + prog + '''"#;
'''
open(p, "w").write(s)
