"""#3274 review: a task with ROOT (main's root, whose end is its own) counts no owner-only
promises: `parks` never reads its count."""
H = "/Users/andii/dev/velt/branches/parked-task/std/lang/hooks/"
EDITS = {
    "sched_node.velt": [
        (
            "    const listing = owned && (fl & lost.LISTED) === 0 ? lost.LISTED : 0;",
            "    const listing = owned && (fl & lost.LISTED) === 0 && !t.has(task, t.ROOT) ? lost.LISTED : 0;",
        ),
    ],
    "sched_node_lost.velt": [
        (
            "  if ((was & lt.LISTED) === 0) {\n    lt.list(task, n);\n  }",
            "  // Not for `main`'s root, whose park is never looked at (sched_task_lost.velt `parks`).\n  if ((was & lt.LISTED) === 0 && !t.has(task, t.ROOT)) {\n    lt.list(task, n);\n  }",
        ),
        (
            "  if ((fl & lt.LISTED) === 0) {\n    nd.setFlags(n, lt.LISTED);\n  }\n  ownerOnly(task, n, fl);",
            "  if ((fl & lt.LISTED) === 0 && !t.has(task, t.ROOT)) {\n    nd.setFlags(n, lt.LISTED);\n  }\n  ownerOnly(task, n, fl);",
        ),
    ],
}
for name, edits in EDITS.items():
    p = H + name
    s = open(p).read()
    for old, new in edits:
        assert s.count(old) == 1, (name, old[:60])
        s = s.replace(old, new)
    open(p, "w").write(s)
print("rootgate")
