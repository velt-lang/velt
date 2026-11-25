#!/bin/bash
# brk.sh <break> <runs> <pause> prog...: builds each program against a std copy with the break and
# runs it counted at 4 and 8 workers under a 60 s cap; one line per run.
set -u
W=/Users/andii/dev/velt/branches/s13-work
export VELT_RT_LIB=/Users/andii/dev/velt/branches/s13-target/debug/libvelt_rt.a
T=/Users/andii/dev/velt/branches/s13-target
TREE=/Users/andii/dev/velt/branches/let-go-knobs
b=$1; runs=$2; pause=$3; shift 3
S=$W/std-$b
[ -d $S ] || python3 $W/$( [[ $b == p_* ]] && echo mkbreak2.py || echo mkbreak.py) $TREE/std $S $b || exit 1
mkdir -p $W/out-$b
for p in "$@"; do
  exe=$W/out-$b/$p
  VELT_STD=$S VELT_SCHEDULER=std perl -e 'alarm shift; exec @ARGV' 600 $T/debug/velt build $W/progs/$p.velt -o $exe > $W/out-$b/$p.build 2>&1 || { echo "$b $p BUILD FAILED"; tail -5 $W/out-$b/$p.build; continue; }
  for th in 4 8; do
    for i in $(seq 1 $runs); do
      VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 VELT_SCHED_TASK_PAUSE=$pause perl -e 'alarm shift; exec @ARGV' 60 $exe > $W/out-$b/$p.$th.$i.out 2> $W/out-$b/$p.$th.$i.err
      c=$?
      echo "$b $p T$th #$i exit $c out=$(tr '\n' ' ' < $W/out-$b/$p.$th.$i.out | head -c 80) $(grep -o 'blocks=[0-9/]*' $W/out-$b/$p.$th.$i.err) $(grep -m1 -o 'ICE.*\|debug-alloc.*\|panic.*' $W/out-$b/$p.$th.$i.err | head -c 120)"
    done
  done
done
