#!/bin/bash
# The two pinned probes, N runs each at 4 workers in a counted run, with the fixed std and with a
# copy whose `away` check always says "here" (the behaviour before the fix, assertions kept).
cd "$(dirname "$0")" || exit 1
runs=${1:-10}
V=/Users/andii/dev/velt/branches/sleep-home/target/debug/velt
FIX=/Users/andii/dev/velt/branches/sleep-home/std
export SDKROOT=$(xcrun --show-sdk-path)
rm -rf std-broken; cp -R "$FIX" std-broken
python3 "$PWD/break.py" "$PWD/std-broken/lang/hooks/sched_sleep.velt" || exit 1
for label in fix broken; do
  std=$FIX; [ $label = broken ] && std=$PWD/std-broken
  for n in ${PROBES:-pinned_poll pinned_drop}; do
    VELT_STD=$std VELT_SCHEDULER=std "$V" build "$n.velt" -o "./$n.$label" > "$n.$label.build" 2>&1 || { echo "$label $n: build failed"; head -5 "$n.$label.build"; continue; }
    for i in $(seq "$runs"); do
      start=$(date +%s)
      VELT_THREADS=4 VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "./$n.$label" > "$n.$label.out" 2> "$n.$label.err" &
      pid=$!; ( sleep 20; kill -9 $pid 2>/dev/null ) & killer=$!
      wait $pid; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
      bal=$(grep -o 'blocks=[0-9]*/[0-9]*' "$n.$label.err" | awk -F'[=/]' '{print ($2==$3)?"bal":"UNBAL"}')
      echo "code=$code $bal $(grep -o 'ICE: [a-z ]*' "$n.$label.err" | head -1 | cut -c1-60) | $(tr '\n' '|' < "$n.$label.out") $(( $(date +%s) - start < 5 ? 0 : 1 ))slow"
    done | sort | uniq -c | sed "s/^/$label $n: /"
  done
done
