# Copies std to <dst> and applies break <name>.
import shutil,sys
src,dst,name=sys.argv[1],sys.argv[2],sys.argv[3]
shutil.copytree(src,dst)
B={
 'head':[],
 'await':[('lang/hooks/sched_node_await.velt','nt !== 0 && wk.taskOf(cw) === nt','wk.taskOf(cw) === nt')],
 'poll':[('lang/hooks/sched_node.velt','    task !== 0 &&\n    wk.taskOf(m.ld(cx + 8)) === task','    wk.taskOf(m.ld(cx + 8)) === task')],
 'nested':[('lang/hooks/sched_node_lost.velt','    task === 0 ||\n',''),],
 'ownside':[('lang/hooks/sched_node_await.velt','return (fl & nd.FINISHED) !== 0;','return true;')],
 'refs':[('lang/hooks/sched_node_done.velt','m.load(n + nd.N_REFS) === refs && ','')],
 'nopause':[('lang/hooks/sched_wake_pause.velt','pause(TASK_PAUSE.load("relaxed"));','')],
}
for f,o,n in B[name]:
    p=f"{dst}/{f}"
    s=open(p).read()
    assert s.count(o)==1,(f,o)
    open(p,'w').write(s.replace(o,n))
