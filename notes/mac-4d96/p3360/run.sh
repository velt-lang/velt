#!/bin/bash
# #3360: the two-dropper program with main's std and with the fix, each also "widened" (a 0.3 ms
# spin just before the closing brace of fulfillLater, i.e. after the latch is open and while the
# frame is still alive). N counted runs at 4 and 8 workers.
#   bash run.sh <runs> [program=stress_two]
cd "$(dirname "$0")" || exit 1
runs=$1; prog=${2:-stress_two}
B=/Users/andii/dev/velt/branches
V=$B/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
cp $B/sleep-combo-var/p3323/$prog.velt . 2>/dev/null
python3 mk.py "$PWD" || exit 1
for label in ${LABELS:-main-wide fix-wide fix}; do
  VELT_STD=$PWD/std-$label VELT_SCHEDULER=std "$V" build $prog.velt -o ./$prog.$label > build.$label.log 2>&1 || { echo "$label: build failed"; head -8 build.$label.log; continue; }
  for th in 4 8; do
    for i in $(seq "$runs"); do
      VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 ./$prog.$label > out.txt 2> err.txt &
      pid=$!; ( sleep 60; kill -9 $pid 2>/dev/null ) & killer=$!
      wait $pid 2>/dev/null; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
      echo "code=$code $(grep -o "blocks=[0-9]*/[0-9]*" err.txt | tail -1 | awk -F"[=/]" "{print (\$2==\$3)?\"bal\":\"LEFT\"}") $(grep -o 'ICE: [a-z A-Z-]*' err.txt | head -1 | cut -c1-50) | $(head -1 out.txt)"
    done | sort | uniq -c | sed "s/^/$label T$th: /"
  done
done
