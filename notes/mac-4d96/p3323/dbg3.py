"""Scratch std: at a node's first hand-over, stops if it is unfinished and has more references
than its owner's and its task's: python3 dbg3.py <out-dir>"""
import shutil
import sys

src = "/Users/andii/dev/velt/branches/parked-away/std"
dst = sys.argv[1]
shutil.rmtree(dst, ignore_errors=True)
shutil.copytree(src, dst)
p = dst + "/lang/hooks/sched_node_transfer.velt"
s = open(p).read()
old = "    m.st(x.make(n) + x.X_TRANSFER, glue);\n"
assert s.count(old) == 1
s = s.replace(
    old,
    old
    + "    if ((m.load32(n + nd.N_FLAGS) & nd.FINISHED) === 0 && m.load(n + nd.N_REFS) !== 2) {\n"
    + "      panic(`ICE: probe: handed over with refs=${m.load(n + nd.N_REFS)} glue=${glue === 0 ? 0 : 1}`);\n"
    + "    }\n",
)
open(p, "w").write(s)
