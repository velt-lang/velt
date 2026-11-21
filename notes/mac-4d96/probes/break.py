"""Scratch variants of sleep-home's sched_sleep.velt.
  python3 break.py <sched_sleep.velt> [broken|fastcheck]
broken (default): the std before the fix's routing, assertions kept: a poll through the slow way
  counts as at home (`here` never says no) and a drop is never sent away.
fastcheck: an ICE when the fast path (the context names the owner) is taken on another worker
  than the one that holds the sleep, the path that skips the hand-over."""
import sys

p = sys.argv[1]
kind = sys.argv[2] if len(sys.argv) > 2 else "broken"
s = open(p).read()
if kind == "broken":
    drop = "    if (s === CARRIED || m.load(f + S_HOME) !== e) {\n      // Its worker takes it off"
    assert s.count(drop) == 1
    s = s.replace(drop, "    if (false) {\n      // Its worker takes it off")
    poll = "  if (s === CARRIED || m.load(f + S_HOME) !== e) {\n    return false;\n  }\n"
    assert s.count(poll) == 1
    s = s.replace(poll, "  if (false) {\n    return false;\n  }\n")
else:
    fast = "      return mv.ask(f, w);\n    }\n"
    assert s.count(fast) == 1
    s = s.replace(
        fast,
        fast
        + "    if (m.load(f + S_HOME) !== t.exec() || s === CARRIED) {\n"
        + '      panic("ICE: probe: fast: a sleep is polled by its owner off its worker");\n'
        + "    }\n",
    )
open(p, "w").write(s)
