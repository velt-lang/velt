#!/bin/bash
# #3323 probes: each built with the base std (parked-task as pushed, from git) and the head std
# (the parked-away worktree), N counted runs at 4 and 8 workers; outcomes.
#   bash run.sh <runs> <probe>...
cd "$(dirname "$0")" || exit 1
runs=$1; shift
B=/Users/andii/dev/velt/branches
V=$B/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
if [ ! -d std-base ]; then
  mkdir -p std-base && git -C $B/parked-away archive parked-task std | tar -x -C std-base
fi
for label in ${LABELS:-base head}; do
  std=$PWD/std-base/std; [ $label = head ] && std=$B/parked-away/std
  [ -n "${STD:-}" ] && [ $label = head ] && std=$STD
  for n in "$@"; do
    VELT_STD=$std VELT_SCHEDULER=std "$V" build "$n.velt" -o "./$n.$label" > "$n.$label.build" 2>&1 || { echo "$label $n: build failed"; head -8 "$n.$label.build"; continue; }
    for th in 4 8; do
      for i in $(seq "$runs"); do
        VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "./$n.$label" > "$n.$label.out" 2> "$n.$label.err" &
        pid=$!; ( sleep 30; kill -9 $pid 2>/dev/null ) & killer=$!
        wait $pid 2>/dev/null; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
        b=$(grep -o 'blocks=[0-9]*/[0-9]*' "$n.$label.err" | tail -1)
        bal=$(echo "$b" | awk -F'[=/]' '{print ($2==$3)?"bal":"LEFT " $2-$3}')
        echo "code=$code $bal $(grep -o 'ICE: [a-z A-Z-]*' "$n.$label.err" | head -1 | cut -c1-70) | $(tr '\n' '|' < "$n.$label.out" | cut -c1-80)"
      done | sort | uniq -c | sed "s/^/$label $n T$th: /"
    done
  done
done
