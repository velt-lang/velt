import sys
p=sys.argv[1]+'/lang/hooks/sched_node_await.velt'
s=open(p).read()
o='    // The owner runs on the node\'s task (module docs).\n'
assert s.count(o)==1
s=s.replace(o,o+'    if (nt === 0 && lk.mode() !== lk.LK_NONE) {\n      panic("PROBE plain0");\n    }\n')
open(p,'w').write(s)
