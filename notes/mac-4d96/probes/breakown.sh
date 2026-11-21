#!/bin/bash
# Break check for the owner fix on the new main: sleep-home's std, and a copy that keeps an owner
# word whatever waker registers the sleep (as before the fix). The test's program (150 rounds)
# run plain, N runs each at 4 and 8 workers.   bash breakown.sh <runs>
cd "$(dirname "$0")" || exit 1
runs=${1:-12}
B=/Users/andii/dev/velt/branches
V=$B/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
if [ -d std-anyowner ]; then find std-anyowner -type f -delete; fi
mkdir -p std-anyowner && cp -R $B/sleep-home/std/ std-anyowner/
python3 breakown.py "$PWD/std-anyowner/lang/hooks/sched_sleep.velt" || exit 1
for label in fix broken; do
  std=$B/sleep-home/std; [ $label = broken ] && std=$PWD/std-anyowner
  VELT_STD=$std VELT_SCHEDULER=std "$V" build busy150.velt -o ./busy150.$label > busy150.$label.build 2>&1 || { echo "$label: build failed"; head -5 busy150.$label.build; continue; }
  for th in 4 8; do
    for i in $(seq "$runs"); do
      VELT_THREADS=$th ./busy150.$label > b.out 2>/dev/null; echo "code=$? $(head -1 b.out)"
    done | sort | uniq -c | sed "s/^/$label T$th: /"
  done
done
