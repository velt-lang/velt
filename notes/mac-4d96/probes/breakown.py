"""Breaks the owner fix: the owner word is kept for any first registration, a combinator's
waker too (the std before the fix did)."""
import sys

p = sys.argv[1]
s = open(p).read()
old = "    const own = s === NEW && ((cx - w) | OWN_BIT) === t.T_CX;\n"
assert s.count(old) == 1
open(p, "w").write(s.replace(old, "    const own = s === NEW;\n"))
