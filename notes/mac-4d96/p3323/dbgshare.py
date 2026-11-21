"""std-wide with the shared-holder ICE saying which tasks it saw: python3 dbgshare.py <dir>"""
import sys

p = sys.argv[1] + "/std-wide/lang/hooks/sched_share_drop.velt"
s = open(p).read()
old = '    panic("ICE: a shared promise\'s holder dropped on another task than the one it is shared on");\n'
assert s.count(old) == 1
new = (
    "    const ht = m.ld(h + sh.H_TASK);\n"
    "    const hn = m.ld(h + sh.H_NODE);\n"
    "    panic(`ICE: probe: cur state=${m.load32(cur + t.T_STATE)} refs=${m.load(cur + t.T_REFS)} flags=${m.ld32(cur + t.T_FLAGS)} "
    "locals=${m.ld(cur + t.T_LOCALS)} woken=${m.load(cur + t.T_WOKEN) === 0 ? 0 : 1} | hub roles=${m.ld(h + sh.H_ROLES)} flags=${m.load32(h + sh.H_FLAGS)} "
    "node=${hn === 0 ? 0 : 1} nodeflags=${hn === 0 ? 0 : m.load32(hn + nd.N_FLAGS)} nodetask=${hn === 0 ? 0 : m.ld(hn + nd.N_TASK) === cur ? 1 : m.ld(hn + nd.N_TASK) === ht ? 2 : 3} hubtaskstate=${m.load32(ht + t.T_STATE)}`);\n"
)
open(p, "w").write(s.replace(old, new))
