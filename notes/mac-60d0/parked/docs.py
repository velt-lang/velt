"""#3274: sched_task_lost.velt's prose says count (and the counted runs' links) for list."""
P = "/Users/andii/dev/velt/branches/parked-task/std/lang/hooks/sched_task_lost.velt"
s = open(P).read()
EDITS = [
    (
        """// The list is touched only by the thread polling the task. A listed promise leaves it as it
// finishes (sched_node_transfer.velt `finished`, sched_node_lost.velt `giveUp`), which is always
// in a poll of its task, and a promise is freed only after it finished, so the list never holds
// a freed one, whichever thread drops the owner's handle.""",
        """// The count (and the links) are touched only by the thread polling the task. A listed promise
// leaves them as it finishes (sched_node_transfer.velt `finished`, sched_node_lost.velt
// `giveUp`), which is always in a poll of its task, and a promise is freed only after it
// finished, so the links never hold a freed one, whichever thread drops the owner's handle.""",
    ),
    ("//   suspended: that drop cannot reach this list or give the promise up (both are the task's", "//   suspended: that drop cannot reach this count or give the promise up (both are the task's"),
    ("/** A node's flag: it is on its task's list (`list`), until it finishes. */", "/** A node's flag: it is counted owner-only on its task (`list`), from then on. */"),
    (
        "// something could still wake it would lose work silently, and a list out of step with its",
        "// something could still wake it would lose work silently, and a count out of step with its",
    ),
]
for old, new in EDITS:
    assert s.count(old) == 1, old[:60]
    s = s.replace(old, new)
open(P, "w").write(s)
print("docs")
