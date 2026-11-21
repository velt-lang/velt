"""Adds the busy-holder test to std_scheduler_sleep_handed.rs: python3 addtest.py <worktree>"""
import sys

wt = sys.argv[1]
p = wt + "/crates/veltc/tests/std_scheduler_sleep_handed.rs"
s = open(p).read()
prog = open("/Users/andii/dev/velt/branches/sleep-combo-var/probes/race_busy.velt").read()
prog = prog.replace("i < 300", "i < 40")
assert "i < 40" in prog
test = '''/// A request for a sleep reaches worker 1 while it is busy in one long poll, and the sleep's
/// deadline passes meanwhile: when the worker runs out of work, its park looks at its timers
/// first (the sleep expires there) and serves the request after, with its park word up, where it
/// looks at its inbox. The task that asked finds the sleep seen, every round. A park that did not
/// serve its requests there would never sleep and never serve (the run's limit ends it).
#[test]
fn a_sleep_asked_of_a_busy_worker_past_its_deadline_is_given_up_at_its_park() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let exe = build(dir.path(), "busy_holder", BUSY_HOLDER);
    check(&exe, "4", 4, "awaited on worker 2: 40\\n");
    check(&exe, "8", 4, "awaited on worker 2: 40\\n");
}

'''
anchor = "/// The program of [`a_sleep_awaited_on_another_worker_moves_to_it`].\n"
assert s.count(anchor) == 1
s = s.replace(anchor, test + anchor)
s = s.rstrip("\n") + (
    "\n\n/// The program of [`a_sleep_asked_of_a_busy_worker_past_its_deadline_is_given_up_at_its_park`].\n"
    'const BUSY_HOLDER: &str = r#"' + prog + '"#;\n'
)
open(p, "w").write(s)
