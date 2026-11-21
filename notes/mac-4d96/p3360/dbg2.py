"""std-move copy (std-dbg2) that stops when a shared promise's holder is handed over on a task
that is not the hub's, saying what it is: python3 dbg2.py <dir>"""
import shutil
import sys

d = sys.argv[1]
shutil.rmtree(d + "/std-dbg2", ignore_errors=True)
shutil.copytree(d + "/std-move", d + "/std-dbg2")
p = d + "/std-dbg2/lang/hooks/sched_share_hand.velt"
s = open(p).read()
old = "export function handOver(f: usize, glue: usize): void {\n  const fwd = sh.forwardOf(f);\n"
assert s.count(old) == 1
s = s.replace(
    old,
    "export function handOver(f: usize, glue: usize): void {\n"
    "  const h0 = m.ld(f - 8);\n"
    "  const cur0 = m.ld(t.exec() + t.E_CURRENT);\n"
    "  if (h0 !== 0 && cur0 !== m.ld(h0 + sh.H_TASK)) {\n"
    "    panic(`ICE: probe: handOver off the hub's task: orig=${sh.isOrig(h0, f)} roles=${m.ld(h0 + sh.H_ROLES)} hubflags=${m.load32(h0 + sh.H_FLAGS)} cur root=${cur0 === 0 ? false : t.has(cur0, t.ROOT)} cur=${cur0 === 0 ? 0 : 1}`);\n"
    "  }\n"
    "  if (h0 === 0) {\n"
    "    panic(`ICE: probe: handOver of a forwarded holder: cur root=${cur0 === 0 ? false : t.has(cur0, t.ROOT)}`);\n"
    "  }\n"
    "  const fwd = sh.forwardOf(f);\n",
)
open(p, "w").write(s)
