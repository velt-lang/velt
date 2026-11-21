#!/bin/bash
# One probe built with a std tree, run N times at 4 workers in counted runs, outcomes tallied.
#   bash probe1.sh <label> <std-dir> <probe> [runs=10]
cd "$(dirname "$0")" || exit 1
label=$1; std=$2; n=$3; runs=${4:-10}
V=/Users/andii/dev/velt/branches/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
VELT_STD=$std VELT_SCHEDULER=std "$V" build "$n.velt" -o "./$n.$label" > "$n.$label.build" 2>&1 || { echo "$label $n: build failed"; head -5 "$n.$label.build"; exit 1; }
for i in $(seq 1 "$runs"); do
  VELT_THREADS=4 VELT_RC_STATS=1 VELT_RT_DEBUG_ALLOC=1 "./$n.$label" > "$n.$label.out" 2> "$n.$label.err" &
  pid=$!; ( sleep 20; kill -9 $pid 2>/dev/null ) & killer=$!
  wait $pid 2>/dev/null; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
  bal=$(grep -o 'blocks=[0-9]*/[0-9]*' "$n.$label.err" | awk -F'[=/]' '{print ($2==$3)?"bal":"UNBAL"}')
  echo "code=$code $bal | $(head -1 "$n.$label.out") | $(grep -o 'ICE: [^|]*' "$n.$label.err" | head -1 | cut -c1-70)"
done | sort | uniq -c | sed "s/^/$label $n: /"
