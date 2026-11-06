#!/usr/bin/env bash
# usage: buildrows.sh LABEL HOME [env...]: instructions of `velt build` (debug), velt process and all processes.
L=$1; H=$2; shift 2
V=~/velt-build/debug-inline-m/release/velt; S=~/velt/branches/debug-inline-m/std; R=~/velt-build/debug-inline/release/libvelt_rt.a
M=~/velt-build/debug-inline/mp; W=$M/bw/$L; mkdir -p $W
for f in $M/p/hello.velt $M/p/log_i64.velt $M/p/lc_stdport.velt ~/velt/branches/debug-inline-m/bench/compile/all_std.velt $SIGX; do n=$(basename $f .velt)
  env "$@" VELT_RT_LIB=$R VELT_STD=$S VELT_HOME=$H VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --trace-children=yes --cachegrind-out-file=/dev/null --log-file=$W/$n.%p.log $V build $f -o $W/$n > /dev/null 2>&1
  python3 - "$L" "$n" $W/$n.*.log <<'P'
import re,sys
L,n=sys.argv[1:3]; tot=0; velt=0; np=0
for p in sys.argv[3:]:
    t=open(p,errors='replace').read()
    m=re.search(r'I\s+refs:\s+([\d,]+)',t)
    if not m: continue
    c=int(m.group(1).replace(',','')); tot+=c; np+=1
    if re.search(r'Command:\s+\S*/velt ',t): velt=c
print(f"{L}\t{n}\tvelt={velt:,}\tall={tot:,}\tprocs={np}")
P
  python3 -c "import glob,os; [os.unlink(p) for p in glob.glob('$W/$n.*.log')]"
done
