import sys,subprocess
# mkcombo <dst> <break> : copy with break + nopause
dst,b=sys.argv[1],sys.argv[2]
subprocess.check_call(['python3','/Users/andii/dev/velt/branches/s13-work/mkbreak.py','/Users/andii/dev/velt/branches/let-go-knobs/std',dst,b])
p=dst+'/lang/hooks/sched_wake_pause.velt'
s=open(p).read(); o='pause(TASK_PAUSE.load("relaxed"));'; assert s.count(o)==1
open(p,'w').write(s.replace(o,''))
