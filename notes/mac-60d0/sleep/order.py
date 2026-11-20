"""#3273 (arm64 rows): release stores only where a reader on another worker needs one; expire's
non-task path zeroes the waker before SEEN."""
H = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/"
EDITS = {
    "sched_sleep.velt": [
        (
            "    m.storeRelease(f + S_HOME, e);\n    insert(block(e), f);",
            "    // Plain: `insert` stores the state last, released, and a worker that reads the state\n"
            "    // reads this after it.\n"
            "    m.st(f + S_HOME, e);\n    insert(block(e), f);",
        ),
        (
            "  // Released: a worker that gave this sleep up reads the state, then `S_HOME` (`release`).\n"
            "  m.storeRelease(f + S_STATE, PENDING);",
            "  // Released: the registration this ends (`S_OWNER`, `S_HOME`, the links) comes with the\n"
            "  // state to a task on another worker that reads it (`sleepPoll`, `sleepDrop`, sched_sleep_move.velt\n"
            "  // `release`). The holder's later states (`DUE`, `FIRED`) are plain stores: a worker that reads\n"
            "  // one only asks the holder for the sleep (it is not `SEEN`), which reads it on its own thread,\n"
            "  // and its `S_HOME` is this registration's, which the reader saw with `PENDING` or reads\n"
            "  // stale: an older value names another worker (it asks, the holder answers), and its own\n"
            "  // worker's it stored itself, before the hand-over whose state it stored itself too.\n"
            "  // `SEEN` stays released: a reader that sees it may free the sleep.\n"
            "  m.storeRelease(f + S_STATE, PENDING);",
        ),
        (
            "  m.st(f + S_TASK, k);\n  m.storeRelease(f + S_STATE, DUE);",
            "  m.st(f + S_TASK, k);\n  // Plain: see `insert`.\n  m.st(f + S_STATE, DUE);",
        ),
        (
            "  unlinkDue(k, f);\n  m.storeRelease(f + S_STATE, FIRED);",
            "  unlinkDue(k, f);\n  // Plain: see `insert`.\n  m.st(f + S_STATE, FIRED);",
        ),
        (
            "      // Polled by a waker that is not a task's: fired by waking it.\n"
            "      m.storeRelease(f + S_STATE, SEEN);\n"
            "      m.st(f + S_WAKER, 0);\n",
            "      // Polled by a waker that is not a task's: fired by waking it. The waker goes first: a\n"
            "      // drop that sees `SEEN` frees the sleep and drops the waker it finds.\n"
            "      m.st(f + S_WAKER, 0);\n"
            "      m.storeRelease(f + S_STATE, SEEN);\n",
        ),
    ],
    "sched_sleep_carry.velt": [
        (
            "    m.storeRelease(f + sl.S_HOME, e);\n    sl.insert(b, f);",
            "    // Plain: `insert` stores the state after it, released.\n    m.st(f + sl.S_HOME, e);\n    sl.insert(b, f);",
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
print("ordered")
