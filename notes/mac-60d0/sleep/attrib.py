"""Attribution of #3207's per-sleep cost: copies of sleep-home's std, each with one piece of the
change taken out (scratch only; the copies are not correct programs for handed-on sleeps).
  python3 attrib.py <out-prefix>   -> <out-prefix>-<variant>"""
import shutil
import sys

SRC = "/Users/andii/dev/velt/branches/sleep-home/std"
out = sys.argv[1]


def variant(name, edits):
    dst = f"{out}-{name}"
    shutil.rmtree(dst, ignore_errors=True)
    shutil.copytree(SRC, dst)
    for rel, old, new in edits:
        p = dst + "/lang/hooks/" + rel
        s = open(p).read()
        assert s.count(old) >= 1, (name, rel, old[:50])
        open(p, "w").write(s.replace(old, new))
    print(dst)


PARK = [
    ("sched_park.velt", "  if (m.load(e + t.E_SLEEP_ASKS) !== 0) {\n    // Before its timer look: a sleep it gives up does not expire here.\n    mv.serve(e);\n  }\n", ""),
    ("sched_park.velt", " || m.load(e + t.E_SLEEP_ASKS) !== 0", ""),
    ("sched_tick.velt", "  if (m.load(e + t.E_SLEEP_ASKS) !== 0) {\n    mv.serve(e);\n  }\n", ""),
    ("sched_end.velt", "  if (m.load(w + t.E_SLEEP_ASKS) !== 0) {", "  if (false) {"),
]
DROP = [("sched_sleep.velt", "  if (m.load(f + S_ASKS) !== 0) {", "  if (false) {")]
CHECK = [
    ("sched_sleep.velt", "  const check = wk.counting();", "  const check = false;"),
    ("sched_sleep.velt", "    if (wk.counting() && m.ld(f + S_HOME) !== e) {", "    if (false) {"),
]
POLL = [("sched_sleep.velt", "    if ((wk.taskOf(w) !== owner || s === CARRIED) && !here(f, s, owner)) {", "    if (false) {")]
STORES = [
    ("sched_sleep.velt", "m.storeRelease(f + S_STATE, PENDING);", "m.st(f + S_STATE, PENDING);"),
    ("sched_sleep.velt", "    m.storeRelease(f + S_HOME, e);", "    m.st(f + S_HOME, e);"),
    ("sched_sleep.velt", "const s = m.load(f + S_STATE);", "const s = m.ld(f + S_STATE);"),
]
variant("full", [])
variant("nopark", PARK)
variant("nodropasks", DROP)
variant("nocheck", CHECK)
variant("nopoll", POLL)
variant("plainstores", STORES)
variant("none", PARK + DROP + CHECK + POLL + STORES)
