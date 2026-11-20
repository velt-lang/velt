"""#3274: the finish takes the node's task from its caller, which has it, for a listed node's
count (no load of N_TASK)."""
H = "/Users/andii/dev/velt/branches/parked-task/std/lang/hooks/"
EDITS = {
    "sched_node_transfer.velt": [
        (
            "/** Node `n`'s state finished: `finished`, then its flags `fl` (FINISHED, maybe DELIVERED) show. */\n@inline(\"always\")\nexport function finish(n: usize, fl: u32): void {\n  finished(n);",
            "/**\n * Node `n` of task `task` (its `N_TASK`) finished: `finished`, then its flags `fl` (FINISHED,\n * maybe DELIVERED) show.\n */\n@inline(\"always\")\nexport function finish(n: usize, task: usize, fl: u32): void {\n  finished(n, task);",
        ),
        (
            " * A node on its task's list of promises only their owner can run leaves it here\n * (sched_task_lost.velt): one test for both, as neither is common on a finish that never\n * suspended under its owner's poll.\n */\n@inline(\"always\")\nexport function finished(n: usize): void {",
            " * A node counted among its task's promises only their owner can run (`task`, its `N_TASK`, which\n * the caller has) leaves the count here (sched_task_lost.velt): one test for both, as neither is\n * common on a finish that never suspended under its owner's poll.\n */\n@inline(\"always\")\nexport function finished(n: usize, task: usize): void {",
        ),
        ("      lt.unlist(m.ld(n + nd.N_TASK), n, fl);", "      lt.unlist(task, n, fl);"),
    ],
    "sched_node.velt": [
        ("      tr.finish(n, FINISHED);", "      tr.finish(n, 0, FINISHED);"),
        ("  tr.finish(n, fl);", "  tr.finish(n, task, fl);"),
        ("  tr.finished(n);", "  tr.finished(n, task);"),
    ],
    "sched_share.velt": [("      tr.finished(n);", "      tr.finished(n, m.ld(n + nd.N_TASK));")],
}
for name, edits in EDITS.items():
    p = H + name
    s = open(p).read()
    for old, new in edits:
        assert s.count(old) == 1, (name, old[:60])
        s = s.replace(old, new)
    open(p, "w").write(s)
print("passtask")
