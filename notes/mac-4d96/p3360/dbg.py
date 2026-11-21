"""std-move with the shared-holder ICE saying what the holder is: python3 dbg.py <dir>"""
import sys

p = sys.argv[1] + "/std-move/lang/hooks/sched_share_drop.velt"
s = open(p).read()
old = '    panic("ICE: a shared promise\'s holder dropped on another task than the one it is shared on");\n'
assert s.count(old) == 1
new = (
    "    const ht = m.ld(h + sh.H_TASK);\n"
    "    panic(`ICE: probe: holder orig=${sh.isOrig(h, f)} hubflags=${m.load32(h + sh.H_FLAGS)} roles=${m.ld(h + sh.H_ROLES)} "
    "origfwd=${m.ld(h + sh.H_ORIG_FWD) === 0 ? 0 : 1} cur root=${t.has(cur, t.ROOT)} hubtask root=${t.has(ht, t.ROOT)} hubtask state=${m.load32(ht + t.T_STATE)}`);\n"
)
open(p, "w").write(s.replace(old, new))
