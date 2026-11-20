#!/bin/bash
# #3202's Windows failure on the Mac: sleeper_on_spare (std_scheduler_rescue.rs) built with
# sleep-home's velt and a std tree, N runs at VELT_THREADS=1 VELT_RESCUE=1 as the test runs it
# (30 s cap); the rescue stats tallied (locks-on >= 2 passes).
#   bash spare.sh <label> <std-dir> [runs=20] [load=0]   load=N: N busy loops beside it
cd "$(dirname "$0")" || exit 1
label=$1; std=$2; runs=${3:-20}; load=${4:-0}
V=/Users/andii/dev/velt/branches/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
VELT_STD=$std VELT_SCHEDULER=std "$V" build sleeper_on_spare.velt -o "./spare.$label" > "spare.$label.build" 2>&1 || { echo "$label: build failed"; head -5 "spare.$label.build"; exit 1; }
pids=()
for i in $(seq 1 "$load"); do
  ( end=$((SECONDS + 900)); while [ $SECONDS -lt $end ]; do :; done ) &
  pids+=($!)
done
for i in $(seq 1 "$runs"); do
  VELT_THREADS=1 VELT_RESCUE=1 VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 VELT_RESCUE_STATS=1 "./spare.$label" > "spare.$label.out" 2> "spare.$label.err" &
  pid=$!; ( sleep 30; kill -9 $pid 2>/dev/null ) & killer=$!
  wait $pid 2>/dev/null; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
  l=$(grep -o 'locks-on=[0-9]*' "spare.$label.err"); s=$(grep -o 'spares=[0-9]*' "spare.$label.err")
  echo "code=$code $l $s $( [ "${l#locks-on=}" -ge 2 ] 2>/dev/null && echo pass || echo FAIL)"
done | sort | uniq -c | sed "s/^/$label load=$load: /"
for p in "${pids[@]}"; do kill "$p" 2>/dev/null; done
