#!/bin/bash
# busy40 (the new test's program) built --release with a std tree, N runs at 4 workers, plain
# (no counting, as users run it) and counted; outcomes.   bash rel.sh <label> <std> <runs>
cd "$(dirname "$0")" || exit 1
label=$1 std=$2 runs=$3
V=/Users/andii/dev/velt/branches/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
sed 's/i < 300/i < 40/' race_busy.velt > busy40.velt
VELT_STD=$std VELT_SCHEDULER=std "$V" build --release busy40.velt -o ./busy40r.$label > busy40r.$label.build 2>&1 || { echo "$label: build failed"; tail -5 busy40r.$label.build; exit 1; }
for mode in plain counted; do
  for i in $(seq "$runs"); do
    if [ $mode = counted ]; then
      VELT_THREADS=4 VELT_RC_STATS=1 VELT_RT_DEBUG_ALLOC=1 ./busy40r.$label > rel.out 2> rel.err &
    else
      VELT_THREADS=4 ./busy40r.$label > rel.out 2> rel.err &
    fi
    pid=$!; ( sleep 30; kill -9 $pid 2>/dev/null ) & killer=$!
    wait $pid 2>/dev/null; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
    echo "code=$code $(grep -o 'ICE: [a-z ]*' rel.err | head -1 | cut -c1-70) | $(head -1 rel.out)"
  done | sort | uniq -c | sed "s/^/$label $mode: /"
done
