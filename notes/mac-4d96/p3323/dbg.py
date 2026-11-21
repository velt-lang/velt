"""A scratch copy of parked-away's std whose droppedAway stops with what it saw when the node is
not lone: python3 dbg.py <out-dir>"""
import shutil
import sys

src = "/Users/andii/dev/velt/branches/parked-away/std"
dst = sys.argv[1]
shutil.rmtree(dst, ignore_errors=True)
shutil.copytree(src, dst)
p = dst + "/lang/hooks/sched_node_lost.velt"
s = open(p).read()
old = "  if (!lone || task === 0 || lk.mode() !== lk.LK_ON) {\n"
assert s.count(old) == 1
s = s.replace(
    old,
    "  if (!lone && (fl & nd.FINISHED) === 0) {\n"
    "    panic(`ICE: probe: dropped away not lone: refs=${refs} flags=${fl} mode=${lk.mode()}`);\n"
    "  }\n" + old,
)
open(p, "w").write(s)
