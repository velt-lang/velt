# Probe std copies: panic where a window is reached.
import shutil,sys
src,dst,name=sys.argv[1],sys.argv[2],sys.argv[3]
shutil.copytree(src,dst)
B={
 'p_letgo':[('lang/hooks/sched_node_done.velt','    m.st(n + nd.N_TASK, 0);\n','    panic("PROBE letgo");\n    m.st(n + nd.N_TASK, 0);\n')],
 'p_poll0':[('lang/hooks/sched_node.velt','  wp.beforeTaskRead();\n  const task = m.ld(n + N_TASK);\n','  wp.beforeTaskRead();\n  const task = m.ld(n + N_TASK);\n  if (task === 0) {\n    panic("PROBE poll0");\n  }\n')],
 'p_own0':[('lang/hooks/sched_node_await.velt','  if (nt === 0) {\n','  if (nt === 0 && (fl & nd.FINISHED) === 0) {\n    panic("PROBE own0");\n  }\n  if (nt === 0) {\n')],
 'p_watch0':[('lang/hooks/sched_share_away.velt','    wp.beforeTaskRead();\n','    wp.beforeTaskRead();\n    if (m.ld(m.ld(h + sh.H_NODE) + 48) === 0) {\n      panic("PROBE watch0");\n    }\n')],
}
for f,o,n in B[name]:
    p=f"{dst}/{f}"
    s=open(p).read()
    assert s.count(o)==1,(f,o)
    open(p,'w').write(s.replace(o,n))
