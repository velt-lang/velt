"""#3273 review, items 1, 2 and 4 (std edits on branch sleep-home)."""
H = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/"


def edit(name, pairs):
    s = open(H + name).read()
    for old, new, count in pairs:
        assert s.count(old) == count, (name, old[:60], s.count(old))
        s = s.replace(old, new)
    open(H + name, "w").write(s)


edit(
    "sched_sleep.velt",
    [
        # 1. No transient NEW: the unlinks leave the state to their callers.
        ("    m.st(next + S_PREV, prev);\n  }\n  m.st(f + S_STATE, NEW);\n}", "    m.st(next + S_PREV, prev);\n  }\n}", 1),
        ("  m.st(f + S_TASK, 0);\n  m.st(f + S_STATE, NEW);\n", "  m.st(f + S_TASK, 0);\n", 1),
        ("      m.st(f + S_STATE, SEEN);\n      return 1;\n", "      m.storeRelease(f + S_STATE, SEEN);\n      return 1;\n", 1),
        ("  m.st(b + TM_UNSEEN, m.ld(b + TM_UNSEEN) - 1);\n  m.st(f + S_STATE, SEEN);\n", "  m.st(b + TM_UNSEEN, m.ld(b + TM_UNSEEN) - 1);\n  m.storeRelease(f + S_STATE, SEEN);\n", 1),
        ("  m.st(f + S_STATE, DUE);\n", "  m.storeRelease(f + S_STATE, DUE);\n", 1),
        ("  m.st(f + S_STATE, FIRED);\n", "  m.storeRelease(f + S_STATE, FIRED);\n", 1),
        ("  m.st(f + S_STATE, CARRIED);\n", "  m.storeRelease(f + S_STATE, CARRIED);\n", 1),
        ("      m.st(f + S_STATE, SEEN);\n      m.st(f + S_WAKER, 0);\n", "      m.storeRelease(f + S_STATE, SEEN);\n      m.st(f + S_WAKER, 0);\n", 1),
    ],
)
edit(
    "sched_sleep_move.velt",
    [
        (
            "  // This request named the sleep until here: the last one of a dropped sleep frees it.\n  if (m.dec(f + sl.S_ASKS) === 1 && isGone(f)) {\n    free(f);\n  }\n",
            "  // This request named the sleep until here: the last one of a dropped sleep frees it. Whether\n"
            "  // it was dropped is read BEFORE the count goes down: once this was the last request, the\n"
            "  // task that asked may drop the sleep on its own worker and free it there (it finds no\n"
            "  // request under way), so the sleep is not read again. A sleep is marked dropped only by a\n"
            "  // drop request being served, which is counted itself and frees at its own end; so one not\n"
            "  // marked here is never left for this request to free.\n"
            "  const gone = isGone(f);\n  if (m.dec(f + sl.S_ASKS) === 1 && gone) {\n    free(f);\n  }\n",
            1,
        ),
    ],
)
edit(
    "sched_relax.velt",
    [
        (
            "    if (s === t.R_ASKED) {\n      if (keeps(e)) {",
            "    if (m.load(e + t.E_SLEEP_ASKS) !== 0) {\n"
            "      // A sleep another worker asked this one for or dropped (sched_sleep_move.velt): served\n"
            "      // here too, or its asker would wait for ever and the program's end for the request.\n"
            "      mv.serve(e);\n    }\n"
            "    if (s === t.R_ASKED) {\n      if (keeps(e)) {",
            1,
        ),
        (
            "    if (m.load(e + t.E_INBOX) !== 0 || q.any(e) || m.load32(e + t.E_RETIRED) !== t.R_RETIRED) {",
            "    if (\n      m.load(e + t.E_INBOX) !== 0 ||\n      m.load(e + t.E_SLEEP_ASKS) !== 0 ||\n      q.any(e) ||\n      m.load32(e + t.E_RETIRED) !== t.R_RETIRED\n    ) {",
            1,
        ),
        (
            "function keeps(e: usize): bool {\n  const b = m.ld(e + t.E_TIMERS);\n",
            "function keeps(e: usize): bool {\n  if (m.load(e + t.E_SLEEP_ASKS) !== 0) {\n    // A request that waits for a carried sleep's task (sched_sleep_move.velt).\n    return true;\n  }\n  const b = m.ld(e + t.E_TIMERS);\n",
            1,
        ),
    ],
)
print("review1 applied")
