"""Mutants of sched_sleep_move.velt `release` in a std copy (measurement/test checks only).
  python3 mutrel.py <std-dir> due|fired|saydue|sayfired|saypending
due: a due sleep is taken off as if pending (its task's due list keeps it).
fired: a fired sleep is handed over without being counted seen on its worker.
say<state>: a panic when release finds that state (to see which states a probe reaches)."""
import sys

p = sys.argv[1] + "/lang/hooks/sched_sleep_move.velt"
kind = sys.argv[2]
s = open(p).read()
if kind == "due":
    old = "  } else if (s === sl.DUE) {\n    sl.unlinkDue(m.ld(f + sl.S_TASK), f);\n"
    new = "  } else if (s === sl.DUE) {\n    sl.unlinkPending(sl.block(e), f);\n"
elif kind == "fired":
    old = "  } else if (s === sl.FIRED) {\n    sl.seenOn(e, f);\n"
    new = "  } else if (s === sl.FIRED) {\n"
else:
    state = kind[3:].upper()
    old = "  if (s === sl.CARRIED) {\n    return 0;\n  }\n"
    new = old + f'  if (s === sl.{state}) {{\n    panic("probe: release finds {state}");\n  }}\n'
assert s.count(old) == 1
open(p, "w").write(s.replace(old, new))
