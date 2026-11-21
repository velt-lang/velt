#!/bin/bash
# Every probe built with a std tree and run plain (no counting, the shipped allocator), N runs at
# 4 and 8 workers: exit codes and first output line.   bash plain.sh <label> <std> <runs> <probe>...
cd "$(dirname "$0")" || exit 1
label=$1 std=$2 runs=$3; shift 3
V=/Users/andii/dev/velt/branches/sleep-home/target/debug/velt
export VELT_STD=$std SDKROOT=$(xcrun --show-sdk-path)
for n in "$@"; do
  VELT_SCHEDULER=std "$V" build "$n.velt" -o "./$n.p$label" > "$n.p$label.build" 2>&1 || { echo "$label $n: build failed"; continue; }
  for th in 4 8; do
    for i in $(seq "$runs"); do
      VELT_THREADS=$th "./$n.p$label" > plain.out 2> plain.err &
      pid=$!; ( sleep 60; kill -9 $pid 2>/dev/null ) & killer=$!
      wait $pid 2>/dev/null; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
      echo "code=$code $(head -1 plain.out)"
    done | sort | uniq -c | sed "s/^/$label $n T$th: /"
  done
done
