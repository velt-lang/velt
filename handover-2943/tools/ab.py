# ab.py N prog...: interleaved lazy vs disk `velt check -v`, best-of-N wall and median instructions.
import subprocess,sys,re,statistics,os
S='/Users/andii/dev/velt/branches/handovers/mac-e688-2943'
N=int(sys.argv[1])
def run(mode,p):
    out=subprocess.run(['bash',S+'/one.sh',mode,p],capture_output=True,text=True).stdout
    t=float(re.search(r'velt: total\s+([0-9.]+)',out).group(1)); i=int(re.search(r'(\d+)\s+instructions retired',out).group(1))
    return t,i
for p in sys.argv[2:]:
    L=[];D=[]
    for _ in range(N):
        L.append(run('lazy',p)); D.append(run('disk',p))
    lt=min(x[0] for x in L); dt=min(x[0] for x in D); li=statistics.median(x[1] for x in L)/1e9; di=statistics.median(x[1] for x in D)/1e9
    print(f"{os.path.basename(p):18} lazy {lt:7.1f} ms {li:5.2f} G | disk {dt:7.1f} ms {di:5.2f} G | {lt/dt:5.2f}x time {li/di:5.2f}x instr",flush=True)
