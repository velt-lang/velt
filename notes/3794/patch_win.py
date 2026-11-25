import sys
p=sys.argv[1]+'/lang/hooks/sched_node.velt'
s=open(p).read()
o='  wp.beforeTaskRead();\n  const task = m.ld(n + N_TASK);\n'
assert s.count(o)==1
s=s.replace(o,o+'''  if (wk.taskOf(m.ld(cx + 8)) === 0 && t.here() !== 0) {
    const now = m.load32(n + N_FLAGS);
    panic(`PROBE fl=${fl} now=${now} task=${task}`);
  }
''')
open(p,'w').write(s)
