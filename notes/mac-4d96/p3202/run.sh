#!/bin/bash
# The rescue test's program (woken_by_spare) with each scratch std, N runs as the test runs it
# (one worker, rescue on, counted): its output against the expected lines, and whether worker 0
# was lazy again (locks-on >= 2).   bash run.sh <runs> <variant...>
cd "$(dirname "$0")" || exit 1
runs=$1; shift
V=/Users/andii/dev/velt/branches/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
python3 mk.py "$PWD" > /dev/null || exit 1
want=$(for r in 0 1 2 3 4; do printf 'task %s before\ntask %s after\nmain %s moved\nmain %s woken\n' $r $r $r $r; done)
for v in "$@"; do
  VELT_STD=$PWD/std-$v VELT_SCHEDULER=std "$V" build woken_by_spare.velt -o ./woken.$v > build.$v.log 2>&1 || { echo "$v: build failed"; head -5 build.$v.log; continue; }
  for i in $(seq "$runs"); do
    VELT_THREADS=1 VELT_RESCUE=1 VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 VELT_RESCUE_STATS=1 ./woken.$v > out.$v.txt 2> err.$v.txt &
    pid=$!; ( sleep 60; kill -9 $pid 2>/dev/null ) & killer=$!
    wait $pid 2>/dev/null; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
    same=DIFF; [ "$(cat out.$v.txt)" = "$want" ] && same=same
    locks=$(grep -o 'locks-on=[0-9]*' err.$v.txt | tail -1 | cut -d= -f2)
    lazy=NOT-LAZY-AGAIN; [ "${locks:-0}" -ge 2 ] && lazy=lazy-again
    echo "code=$code out=$same $lazy"
  done | sort | uniq -c | sed "s/^/$v: /"
done
