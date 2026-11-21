"""Second round of #3273 cut candidates on a std tree: python3 cut2.py <std> <lazyw|fired|noexp|nohome>"""
import sys

std, which = sys.argv[1], sys.argv[2]
p = f"{std}/lang/hooks/sched_sleep.velt"
s = open(p).read()


def rep(old, new):
    global s
    assert s.count(old) == 1, (which, old[:70], s.count(old))
    s = s.replace(old, new)


if which in ("lazyw", "fired"):
    rep("  const s = m.load(f + S_STATE);\n  const w = m.ld(cx + 8);\n  if (s <= SEEN) {", "  const s = m.load(f + S_STATE);\n  if (s <= SEEN) {")
    rep("      return mv.ask(f, w);\n", "      return mv.ask(f, m.ld(cx + 8));\n")
    rep("  const old = m.ld(f + S_WAKER);\n  if (old !== w) {", "  const w = m.ld(cx + 8);\n  const old = m.ld(f + S_WAKER);\n  if (old !== w) {")
if which == "fired":
    # A fired sleep's poll registers nothing: it needs only this worker to hold the sleep.
    rep(
        "    if (m.ld(cx) !== m.loadRelaxed(f + S_OWNER) && !here(f, s)) {",
        "    if (s === FIRED) {\n"
        "      const e = t.exec();\n"
        "      if (m.load(f + S_HOME) !== e) {\n"
        "        return mv.ask(f, m.ld(cx + 8));\n"
        "      }\n"
        "      seenOn(e, f);\n"
        "      return 1;\n"
        "    }\n"
        "    if (m.ld(cx) !== m.loadRelaxed(f + S_OWNER) && !here(f, s)) {",
    )
    rep("    if (s === FIRED) {\n      seenOn(t.exec(), f);\n      return 1;\n    }\n", "")
if which == "noexp":
    rep("  if (wk.counting()) {\n    rl.checkExpiring(b, now);\n  }\n", "")
if which == "nohome":
    rep("    m.storeRelaxed(f + S_HOME, e);\n    insert(block(e), f);", "    insert(block(e), f);")
open(p, "w").write(s)
