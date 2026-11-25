import sys
p=sys.argv[1]+'/lang/hooks/sched_node.velt'
s=open(p).read()
o='''    wk.taskOf(m.ld(cx + 8)) === task
  ) {
'''
assert s.count(o)==1
s=s.replace(o,o+'''    if (task === 0) {
      panic(`PROBE adopt0 fl=${fl} now=${m.load32(n + N_FLAGS)}`);
    }
''')
o2='  wp.beforeTaskRead();\n  const task = m.ld(n + N_TASK);\n'
s=s.replace(o2,o2+'''  if (task === 0 && wk.taskOf(m.ld(cx + 8)) === 0) {
    panic(`PROBE seen0 fl=${fl} ad=${m.ld(t.exec() + t.E_ADOPTING)}`);
  }
''')
open(p,'w').write(s)
