"""#3273: S_OWNER holds the owner's await word (its task + T_WORD), what a context's first word
points at, so the poll compares the two directly."""
H = "/Users/andii/dev/velt/branches/sleep-home/std/lang/hooks/"
EDITS = {
    "sched_sleep.velt": [
        (
            "/** The task that polled it into its worker's timer block (sched_sleep_carry.velt `carry`). */",
            "/**\n * The await word (`T_WORD`) of the task that polled it into its worker's timer block, what that\n * task's contexts point at (`sleepPoll`; sched_sleep_carry.velt `carry`), or 0.\n */",
        ),
        (
            "    if (m.ld(cx) - t.T_WORD !== m.loadRelaxed(f + S_OWNER) && !here(f, s)) {",
            "    if (m.ld(cx) !== m.loadRelaxed(f + S_OWNER) && !here(f, s)) {",
        ),
        (
            "    m.storeRelaxed(f + S_OWNER, m.ld(e + t.E_CURRENT));",
            "    m.storeRelaxed(f + S_OWNER, m.ld(e + t.E_CURRENT) + t.T_WORD);",
        ),
        (
            "  if (k !== m.loadRelaxed(f + S_OWNER)) {\n    sp.anchor();\n    m.storeRelaxed(f + S_OWNER, k);",
            "  if (k + t.T_WORD !== m.loadRelaxed(f + S_OWNER)) {\n    sp.anchor();\n    m.storeRelaxed(f + S_OWNER, k + t.T_WORD);",
        ),
    ],
    "sched_sleep_carry.velt": [
        ("    m.storeRelaxed(f + sl.S_OWNER, k);", "    m.storeRelaxed(f + sl.S_OWNER, k + t.T_WORD);"),
        (
            "  const o = m.ld(f + sl.S_OWNER);\n",
            "  const word = m.ld(f + sl.S_OWNER);\n  const o = word === 0 ? 0 : word - t.T_WORD;\n",
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
print("ownerword")
