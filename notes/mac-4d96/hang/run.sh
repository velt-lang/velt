#!/bin/bash
# The SPAWNED program of std_scheduler_all at one worker, built with a std tree: N runs, 20 s cap.
#   bash run.sh <label> <std-dir> <runs>
cd "$(dirname "$0")" || exit 1
label=$1 std=$2 runs=$3
B=/Users/andii/dev/velt/branches
V=$B/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
sed -n '/^const SPAWNED: &str = r#"/,/^"#;/p' $B/sleep-home/crates/veltc/tests/std_scheduler_all.rs | sed -e '1s/^const SPAWNED: &str = r#"//' -e '$d' > spawned.velt
VELT_STD=$std VELT_SCHEDULER=std "$V" build spawned.velt -o ./spawned.$label > build.$label.log 2>&1 || { echo "$label: build failed"; head -5 build.$label.log; exit 1; }
for i in $(seq "$runs"); do
  VELT_THREADS=1 ./spawned.$label > out.$label.txt 2> err.$label.txt &
  pid=$!; ( sleep 20; kill -9 $pid 2>/dev/null ) & killer=$!
  wait $pid 2>/dev/null; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
  echo "code=$code $(head -1 out.$label.txt)"
done | sort | uniq -c | sed "s/^/$label: /"
