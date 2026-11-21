"""Scratch std (from sleep-home's): expire stops, in any run, when it is about to file a sleep
under a task whose home is another worker, saying what that task is: python3 mkwho.py <dir>"""
import shutil
import sys

src = "/Users/andii/dev/velt/branches/sleep-home/std"
dst = sys.argv[1] + "/std-who"
shutil.rmtree(dst, ignore_errors=True)
shutil.copytree(src, dst)
p = dst + "/lang/hooks/sched_sleep.velt"
s = open(p).read()
old = "      pushDue(k, f);\n"
assert s.count(old) == 1
new = (
    "      if (m.ld(k + t.T_HOME) !== t.exec()) {\n"
    "        const e = t.exec();\n"
    "        panic(`ICE: probe: expire for a task of another worker: task state=${m.load32(k + t.T_STATE)} flags=${m.ld32(k + t.T_FLAGS)} "
    "root=${t.has(k, t.ROOT)} refs=${m.load(k + t.T_REFS)} | sleep owner=${m.ld(f + S_OWNER) === k + t.T_WORD ? 1 : 0} home=${m.ld(f + S_HOME) === e ? 1 : 0} asks=${m.load(f + S_ASKS)} "
    "| e is main=${e === t.main() ? 1 : 0} taskhome is main=${m.ld(k + t.T_HOME) === t.main() ? 1 : 0}`);\n"
    "      }\n" + old
)
open(p, "w").write(s.replace(old, new))
print(dst)
