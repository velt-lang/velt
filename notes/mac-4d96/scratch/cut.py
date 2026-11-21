"""Applies one of the #3273 cuts to a std tree: python3 cut.py <std> <park|init|drop|dropw>"""
import sys

std, which = sys.argv[1], sys.argv[2]


def edit(rel, pairs):
    p = f"{std}/lang/hooks/{rel}"
    s = open(p).read()
    for old, new in pairs:
        assert s.count(old) == 1, (rel, old[:60], s.count(old))
        s = s.replace(old, new)
    open(p, "w").write(s)


if which == "park":
    edit(
        "sched_park.velt",
        [
            (
                "  if (m.load(e + t.E_SLEEP_ASKS) !== 0) {\n"
                "    // Before its timer look: a sleep it gives up does not expire here.\n"
                "    mv.serve(e);\n"
                "  }\n",
                "",
            ),
            (
                "  if (m.load(e + t.E_INBOX) !== 0 || m.load(e + t.E_SLEEP_ASKS) !== 0 || q.any(e) || !parkable(e)) {\n"
                "    wk.awake(e);\n",
                "  // The inbox and the sleeps asked for (sched_sleep_move.velt) in one test.\n"
                "  if ((m.load(e + t.E_INBOX) | m.load(e + t.E_SLEEP_ASKS)) !== 0 || q.any(e) || !parkable(e)) {\n"
                "    wk.awake(e);\n"
                "    if (m.load(e + t.E_SLEEP_ASKS) !== 0) {\n"
                "      // Served here, where a worker out of work finds them (a busy one at its I/O tick):\n"
                "      // the park's way in, which every sleep's round takes, looks at nothing for them. A\n"
                "      // sleep asked for may have come due in the timer look above: it is given up from\n"
                "      // its due list as from the pending one (`release`).\n"
                "      mv.serve(e);\n"
                "    }\n",
            ),
        ],
    )
elif which == "init":
    edit(
        "sched_sleep.velt",
        [
            (
                "  m.st(f + S_STATE, NEW);\n"
                "  m.st(f + S_OWNER, 0);\n"
                "  // The links, `S_HOME` and `S_TASK` are written when a state that reads them begins: the links\n"
                "  // and `S_HOME` with `PENDING` (`insert`), `S_TASK` with `DUE` (`pushDue`).\n",
                "  m.st(f + S_STATE, NEW);\n"
                "  // The links, `S_OWNER`, `S_HOME` and `S_TASK` are written when a state that reads them\n"
                "  // begins: the links, `S_OWNER` and `S_HOME` with `PENDING` (`sleepPoll`, `insert`; `adopt`),\n"
                "  // `S_TASK` with `DUE` (`pushDue`).\n",
            ),
        ],
    )
elif which in ("drop", "dropw"):
    test = "((s >> 1) | asks) !== 0" if which == "drop" else "((s >> 1) | asks | m.loadRelaxed(f + S_WAKER)) !== 0"
    tail = (
        "  const w = m.ld(f + S_WAKER);\n  if (w !== 0) {\n    m.wakerDrop(w);\n  }\n  m.release(f, S_SIZE);\n}\n"
        if which == "drop"
        else "  m.release(f, S_SIZE);\n}\n"
    )
    edit(
        "sched_sleep.velt",
        [
            (
                "function sleepDrop(f: usize): void {\n"
                "  const s = m.load(f + S_STATE);\n"
                "  if (m.load(f + S_ASKS) !== 0) {\n",
                "function sleepDrop(f: usize): void {\n"
                "  const s = m.load(f + S_STATE);\n"
                "  const asks = m.load(f + S_ASKS);\n"
                "  // The usual drop in one test: on no list (new or seen, the two states below `PENDING`)\n"
                "  // and named by no request.\n"
                f"  if ({test}) {{\n"
                "    dropHeld(f, s, asks);\n"
                "    return;\n"
                "  }\n" + tail + "\n"
                "/** The drop of a sleep that holds a place (state `s` above `SEEN`) or that requests name (`asks`). */\n"
                "@noInline\n"
                "function dropHeld(f: usize, s: usize, asks: usize): void {\n"
                "  if (asks !== 0) {\n",
            ),
        ],
    )
