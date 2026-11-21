#!/bin/bash
# Break check for the busy-holder test: the cut std, and a copy whose park does not serve its
# sleep requests in the bail branch. 3 runs each at 4 workers, 30 s cap.
cd "$(dirname "$0")" || exit 1
B=/Users/andii/dev/velt/branches
V=$B/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
if [ -d std-nopark ]; then find std-nopark -type f -delete; fi
mkdir -p std-nopark && cp -R $B/sleep-combo/std/ std-nopark/
python3 breakpark.py "$PWD/std-nopark/lang/hooks/sched_park.velt" || exit 1
sed 's/i < 300/i < 40/' race_busy.velt > busy40.velt
for label in fix broken; do
  std=$B/sleep-combo/std; [ $label = broken ] && std=$PWD/std-nopark
  VELT_STD=$std VELT_SCHEDULER=std "$V" build busy40.velt -o ./busy40.$label > busy40.$label.build 2>&1 || { echo "$label: build failed"; head -5 busy40.$label.build; continue; }
  for i in 1 2 3; do
    VELT_THREADS=4 VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 ./busy40.$label > busy40.$label.out 2> busy40.$label.err &
    pid=$!; ( sleep 30; kill -9 $pid 2>/dev/null ) & killer=$!
    wait $pid; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
    echo "$label: code=$code $(grep -o 'blocks=[0-9]*/[0-9]*' busy40.$label.err) | $(tr '\n' '|' < busy40.$label.out)"
  done
done
