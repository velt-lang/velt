"""#3274: whether a listed node is linked is its own flag (NLINKED), read off the flags the
finish already loaded, not the task's."""
H = "/Users/andii/dev/velt/branches/parked-task/std/lang/hooks/"
EDITS = {
    "sched_task_lost.velt": [
        (
            "/** Listed node `n` of task `task` finished or was given up, in a poll of that task. */\n@inline(\"always\")\nexport function unlist(task: usize, n: usize): void {\n  if (t.has(task, t.LINKED)) {",
            "/**\n * Listed node `n` of task `task` (its flags `fl` from before) finished or was given up, in a\n * poll of that task.\n */\n@inline(\"always\")\nexport function unlist(task: usize, n: usize, fl: u32): void {\n  if ((fl & NLINKED) !== 0) {",
        ),
        (
            "function linkCounted(task: usize, n: usize): void {\n  checkLink(task, n);\n",
            "function linkCounted(task: usize, n: usize): void {\n  checkLink(task, n);\n  nd.setFlags(n, NLINKED);\n",
        ),
        (
            "export const LISTED: u32 = 131072;\n",
            "export const LISTED: u32 = 131072;\n\n/** A node's flag: it is linked as well (`list`, with the block counters on). */\nexport const NLINKED: u32 = 262144;\n",
        ),
    ],
    "sched_node_transfer.velt": [("      lt.unlist(m.ld(n + nd.N_TASK), n);", "      lt.unlist(m.ld(n + nd.N_TASK), n, fl);")],
    "sched_node_lost.velt": [
        (
            "  if ((m.or32(n + nd.N_FLAGS, nd.FINISHED) & lt.LISTED) !== 0) {",
            "  const was = m.or32(n + nd.N_FLAGS, nd.FINISHED);\n  if ((was & lt.LISTED) !== 0) {",
        ),
        ("    lt.unlist(task, n);", "    lt.unlist(task, n, was);"),
    ],
}
for name, edits in EDITS.items():
    p = H + name
    s = open(p).read()
    for old, new in edits:
        assert s.count(old) == 1, (name, old[:60])
        s = s.replace(old, new)
    open(p, "w").write(s)
print("nlinked")
