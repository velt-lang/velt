"""Widened-window copies of sched_spread.velt `offer` (theory check only, never committed).
  python3 wideoffer.py <std-dir> old|new
old: the one-at-a-time push loop with a spin after each push (a steal may come between).
new: the one-step push with the same spin before it."""
import sys

p = sys.argv[1] + "/lang/hooks/sched_spread.velt"
kind = sys.argv[2]
s = open(p).read()
spin = "{0}for (let z: usize = 0; z < 200000; z++) {{\n{0}  m.loadRelaxed(e + t.E_OFFER);\n{0}}}\n"
if kind == "old":
    old = "    m.pushStack(e + t.E_OFFER, k, t.T_NEXT);\n"
    assert s.count(old) == 1
    s = s.replace(old, old + spin.format("    "))
else:
    old = "  m.pushChain(e + t.E_OFFER, k, last, t.T_NEXT);\n"
    assert s.count(old) == 1
    s = s.replace(old, spin.format("  ") + old)
open(p, "w").write(s)
