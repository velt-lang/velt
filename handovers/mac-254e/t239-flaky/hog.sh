#!/bin/bash
# usage: hog.sh <hogs> <command...>: runs <command> with <hogs> `yes` processes busy, then kills
# exactly those processes (by PID).
n=$1; shift
pids=()
for i in $(seq 1 "$n"); do
  yes > /dev/null &
  pids+=($!)
done
sleep 3
echo "load: $(uptime | sed 's/.*load averages: //')"
"$@"
kill "${pids[@]}"
wait 2>/dev/null
echo "hogs killed: ${#pids[@]}"
