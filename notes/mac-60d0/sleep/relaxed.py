"""#3273 round 2, item C: words another worker reads are stored and loaded atomically (relaxed
where no order is needed), and insert's doc gives the real reason DUE and FIRED need no release."""
H = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/"
EDITS = {
    "sched_sleep.velt": [
        (
            "    if (m.ld(cx) - t.T_WORD !== m.ld(f + S_OWNER) && !here(f, s)) {",
            "    if (m.ld(cx) - t.T_WORD !== m.loadRelaxed(f + S_OWNER) && !here(f, s)) {",
        ),
        (
            "    m.st(f + S_OWNER, m.ld(e + t.E_CURRENT));\n    // Plain: `insert` stores the state after it, released.\n    m.st(f + S_HOME, e);",
            "    m.storeRelaxed(f + S_OWNER, m.ld(e + t.E_CURRENT));\n    // Unordered: `insert` stores the state after it, released.\n    m.storeRelaxed(f + S_HOME, e);",
        ),
        (
            "  if (k !== m.ld(f + S_OWNER)) {\n    sp.anchor();\n    m.st(f + S_OWNER, k);",
            "  if (k !== m.loadRelaxed(f + S_OWNER)) {\n    sp.anchor();\n    m.storeRelaxed(f + S_OWNER, k);",
        ),
        ("  // Plain: see `insert`.\n  m.st(f + S_STATE, DUE);", "  // Unordered: see `insert`.\n  m.storeRelaxed(f + S_STATE, DUE);"),
        ("  // Plain: see `insert`.\n  m.st(f + S_STATE, FIRED);", "  // Unordered: see `insert`.\n  m.storeRelaxed(f + S_STATE, FIRED);"),
        (
            "  // No owner while it is carried: a poll of it goes the slow way (`sleepPoll`).\n  m.st(f + S_OWNER, 0);",
            "  // No owner while it is carried: a poll of it goes the slow way (`sleepPoll`).\n  m.storeRelaxed(f + S_OWNER, 0);",
        ),
        (
            """ * The state is stored last, released: the registration this ends (`S_OWNER`, `S_HOME`, the
 * links) comes with it to a task on another worker that reads the state (`sleepPoll`,
 * `sleepDrop`, sched_sleep_move.velt `release`). The holder's later states, `DUE` and `FIRED`,
 * are plain stores. A worker that reads one only asks the holder for the sleep (it is not
 * `SEEN`), and the holder reads it on its own thread. Its `S_HOME` is this registration's, seen
 * with `PENDING`, or stale: an older value names another worker (which asks, and the holder
 * answers), or its own worker, which stored that itself, before the hand-over whose state it
 * stored itself too. `SEEN` stays released: a worker that reads it may free the sleep.""",
            """ * The state is stored last, released: the registration this ends (`S_OWNER`, `S_HOME`, the
 * links) comes with it to a worker that reads the state with an acquiring load (`sleepPoll`,
 * `here`, `sleepDrop`, sched_sleep_move.velt `release`). The holder's later states, `DUE` and
 * `FIRED`, are stored unordered (relaxed), and a reader that loads one of them sees this
 * registration all the same: they are stored by the thread that released `PENDING`, to the same
 * word, after it, and both targets keep one thread's stores to one word in that order (x86-64
 * is TSO; arm64 orders a store after an earlier store to the same location from the same thread
 * and is other-multi-copy atomic), while no compiler moves a store above a release store to the
 * same word. The language's release sequences would not carry it (they continue only through
 * read-modify-writes); this is the reason it holds. So a reader that finds its own worker in
 * `S_HOME` while the state is `DUE` or `FIRED` reads this registration's home, never an older
 * one. `SEEN` stays released: a worker that reads it may free the sleep.""",
        ),
    ],
    "sched_sleep_carry.velt": [
        (
            "    m.st(f + sl.S_OWNER, k);\n    // Plain: `insert` stores the state after it, released.\n    m.st(f + sl.S_HOME, e);",
            "    m.storeRelaxed(f + sl.S_OWNER, k);\n    // Unordered: `insert` stores the state after it, released.\n    m.storeRelaxed(f + sl.S_HOME, e);",
        ),
    ],
}
for name, edits in EDITS.items():
    p = H + name
    s = open(p).read()
    for old, new in edits:
        assert s.count(old) == 1, (name, old[:70])
        s = s.replace(old, new)
    open(p, "w").write(s)
print("relaxed")
