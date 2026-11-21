"""#3273: the plain-run test of the reused owner address. python3 addtest3273b.py <worktree>"""
import sys

p = sys.argv[1] + "/crates/veltc/tests/std_scheduler_sleep_handed.rs"
s = open(p).read()


def rep(old, new):
    global s
    assert s.count(old) == 1, (old[:60], s.count(old))
    s = s.replace(old, new)


# run() takes whether the run is counted.
rep(
    "/// Runs `exe` with `threads` workers, counted and with the checking allocator, killed after a\n"
    "/// minute: its exit code (`None` when killed) and output.\n"
    "fn run(exe: &Path, threads: &str) -> (Option<i32>, String) {\n"
    "    let mut child = command(exe)\n"
    "        .env_remove(\"VELT_SCHEDULER\")\n"
    "        .env_remove(\"VELT_RESCUE\")\n"
    "        .env(\"VELT_THREADS\", threads)\n"
    "        .env(\"VELT_RC_STATS\", \"1\")\n"
    "        .env(\"VELT_RT_DEBUG_ALLOC\", \"1\")\n"
    "        .stdout(Stdio::piped())\n",
    "/// Runs `exe` with `threads` workers, counted and with the checking allocator, killed after a\n"
    "/// minute: its exit code (`None` when killed) and output.\n"
    "fn run(exe: &Path, threads: &str) -> (Option<i32>, String) {\n"
    "    run_as(exe, threads, true)\n"
    "}\n\n"
    "/// [`run`], or (`counted` false) a run as a user's: no block counts and the allocator the\n"
    "/// program ships with, which gives a freed block's address to the next one of its size. The\n"
    "/// checking allocator does not, so it hides whatever depends on an address being reused.\n"
    "fn run_as(exe: &Path, threads: &str, counted: bool) -> (Option<i32>, String) {\n"
    "    let mut c = command(exe);\n"
    "    c.env_remove(\"VELT_SCHEDULER\")\n"
    "        .env_remove(\"VELT_RESCUE\")\n"
    "        .env_remove(\"VELT_RC_STATS\")\n"
    "        .env_remove(\"VELT_RT_DEBUG_ALLOC\")\n"
    "        .env(\"VELT_THREADS\", threads);\n"
    "    if counted {\n"
    "        c.env(\"VELT_RC_STATS\", \"1\").env(\"VELT_RT_DEBUG_ALLOC\", \"1\");\n"
    "    }\n"
    "    let mut child = c\n"
    "        .stdout(Stdio::piped())\n",
)

test = '''/// The program of the test above, 150 rounds, run as a user runs it (no checking allocator). In
/// each round the task that polled the sleep into worker 1's block ends, and the task that then
/// awaits the sleep on worker 2 is made right after it: with the shipped allocator it gets the
/// ended task's address. The sleep was registered through `Promise.race`'s waker, which keeps no
/// task alive, and it used to keep the first task's address as its owner all the same: the new
/// task passed for the owner, skipped the hand-over, and worker 1's expiry filed the sleep under
/// a task of worker 2, which fired it with no timer block (a segmentation fault in half the
/// runs at 4 workers and three in four at 8; the counted runs above never saw it). A sleep names
/// an owner only while its own waker keeps that task alive (sched_sleep_move.velt `owner`).
#[test]
fn a_sleep_does_not_take_a_new_task_at_its_first_tasks_address_for_its_owner() {
    if !has_threads() {
        return;
    }
    runtime_support::build_native_runtime(manifest());
    let dir = test_dir::TestDir::new();
    let src = BUSY_HOLDER.replace("i < 40", "i < 150");
    assert!(src.contains("i < 150"));
    let exe = build(dir.path(), "reused_owner", &src);
    for threads in ["4", "8"] {
        for i in 0..8 {
            let (code, out) = run_as(&exe, threads, false);
            assert_eq!(code, Some(0), "run {i} at {threads} workers: {out}");
            assert_eq!(out, "awaited on worker 2: 150\\n", "run {i} at {threads} workers");
        }
    }
}

'''
anchor = "/// The program of [`a_sleep_awaited_on_another_worker_moves_to_it`].\n"
rep(anchor, test + anchor)
open(p, "w").write(s)
