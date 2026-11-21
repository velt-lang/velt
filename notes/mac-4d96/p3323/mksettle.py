"""Scratch copy of the BASE std (#3322's, no #3323 change) whose fulfillLater waits ~300 us
after it settled the slot, before its frame lets go of the value: python3 mksettle.py <dir>"""
import shutil
import sys

src = sys.argv[1] + "/std-base/std"
dst = sys.argv[1] + "/std-base-settle"
shutil.rmtree(dst, ignore_errors=True)
shutil.copytree(src, dst)
p = dst + "/lang/promise/settle.velt"
s = open(p).read()
old = (
    "  const v = transfer(value);\n"
    '  openLatch(guard.with((g) => slot.with((s) => s.settle({ status: "fulfilled", value: v }))));\n'
    "}\n"
)
assert s.count(old) == 1
new = old[:-2] + "  const until = performance.now() + 0.3;\n  while (performance.now() < until) {}\n}\n"
open(p, "w").write(s.replace(old, new))
print(dst)
