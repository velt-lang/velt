#!/bin/bash
# The handed-on sleep probes with cross-wake's debug velt and a std tree: builds each probe,
# runs it N times at 4 and 8 workers with the checking allocator; outcome counts.
#   bash stress.sh <label> <std-dir> <runs> <probe>...
cd "$(dirname "$0")" || exit 1
label=$1 std=$2 runs=$3; shift 3
V=/Users/andii/dev/velt/branches/sleep-home/target/debug/velt
export VELT_STD=$std SDKROOT=$(xcrun --show-sdk-path)
for n in "$@"; do
  exe=./$n.$label
  VELT_SCHEDULER=std "$V" build "$n.velt" -o "$exe" > "$n.$label.build" 2>&1 || { echo "$label $n: build failed"; head -5 "$n.$label.build"; continue; }
  for th in 4 8; do
    out=$n.$label.t$th.all; : > "$out"
    for i in $(seq "$runs"); do
      VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 VELT_RESCUE_STATS=1 "$exe" > "$n.$label.out" 2> "$n.$label.err" &
      pid=$!
      ( sleep 120; kill -9 $pid 2>/dev/null ) & killer=$!
      wait $pid; code=$?
      kill $killer 2>/dev/null; wait $killer 2>/dev/null
      bal=$(grep -o 'blocks=[0-9]*/[0-9]*' "$n.$label.err" | awk -F'[=/]' '{print ($2==$3)?"bal":"UNBAL"}')
      ice=$(grep -o 'ICE: [a-z ]*: [a-z ]*' "$n.$label.err" | head -1)
      echo "code=$code $bal $ice | $(tr "\n" "|" < "$n.$label.out") $(grep -o "asked=.*" "$n.$label.err" | sed "s/[0-9][0-9]*/N/g")" >> "$out"; grep -o "asked=.*" "$n.$label.err" >> "$n.$label.counts"
    done
    echo "== $label $n T$th"; sort "$out" | uniq -c
  done
done
