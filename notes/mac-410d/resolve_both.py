"""Resolves conflict hunks by keeping both sides (ours, then theirs), for files given as arguments."""
import re
import sys

for p in sys.argv[1:]:
    t = open(p).read()
    pat = re.compile(r"<<<<<<< [^\n]*\n(.*?)=======\n(.*?)>>>>>>> [^\n]*\n", re.S)
    t, n = pat.subn(lambda m: m.group(1) + m.group(2), t)
    open(p, "w").write(t)
    print(p, n)
