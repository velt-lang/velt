"""Widened-window std copies for #3273 item B (measurement/test checks only, never committed).
  python3 widegone.py <std-dir> old|new
old: the 7f5cfbf5d code (gone read, then dec) with a spin between the two.
new: the GONE-bit code with the same spin before its one atomic step."""
import sys

p = sys.argv[1] + "/lang/hooks/sched_sleep_move.velt"
kind = sys.argv[2]
s = open(p).read()
spin = "  for (let z: usize = 0; z < 100000; z++) {\n    m.loadRelaxed(f + sl.S_STATE);\n  }\n"
if kind == "old":
    old = "  const gone = isGone(f);\n"
    assert s.count(old) == 1
    s = s.replace(old, old + spin)
else:
    old = "  const left =\n"
    assert s.count(old) == 1
    s = s.replace(old, spin + old)
open(p, "w").write(s)
