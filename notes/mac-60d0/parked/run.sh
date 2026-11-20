#!/bin/bash
# #131: the golden lang/task_parked_freed built with sleep-home's velt (main's compiler) and a
# std tree, run N times per mode in counted runs with the checking allocator; outcomes tallied
# (output equal to the .out, blocks balanced, ICE lines, hangs after 30 s).
#   bash run.sh <label> <std-dir> [runs=10]
cd "$(dirname "$0")" || exit 1
label=$1; std=$2; runs=${3:-10}
W=/Users/andii/dev/velt/branches/parked-task
V=/Users/andii/dev/velt/branches/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
VELT_STD=$std "$V" build "$W/tests/golden/lang/task_parked_freed.velt" -o "./parked.$label" > "build.$label.log" 2>&1 || { echo "$label: build failed"; head -8 "build.$label.log"; exit 1; }
for mode in "VELT_THREADS=1" "VELT_THREADS=4" "VELT_THREADS=8" "VELT_THREADS=1 VELT_RESCUE=1"; do
  for i in $(seq 1 "$runs"); do
    env $mode VELT_RC_STATS=1 VELT_RT_DEBUG_ALLOC=1 "./parked.$label" > "out.$label.txt" 2> "err.$label.txt" &
    pid=$!; ( sleep 30; kill -9 $pid 2>/dev/null ) & killer=$!
    wait $pid; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
    same=DIFF; cmp -s "out.$label.txt" "$W/tests/golden/lang/task_parked_freed.out" && same=same
    bal=$(grep -o 'blocks=[0-9]*/[0-9]*' "err.$label.txt" | awk -F'[=/]' '{print ($2==$3)?"bal":"UNBAL(" $2-$3 ")"}')
    echo "code=$code out=$same $bal $(grep -o 'ICE: [^|]*' "err.$label.txt" | head -1 | cut -c1-70)"
  done | sort | uniq -c | sed "s/^/$label [$mode] /"
done
