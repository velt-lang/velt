#!/bin/bash
# usage: late.sh <exe> <runs> <threads> <parallel>: the pinned timers' lateness over <runs> runs:
# the five largest of each, and a histogram of the first timer's.
exe=$1; runs=$2; th=$3; par=$4
export LATE_EXE=$exe VELT_THREADS=$th
seq 1 "$runs" | xargs -P "$par" -n 1 bash -c '"$LATE_EXE" | grep "^late"' > /tmp/late.$$
echo "runs with output: $(wc -l < /tmp/late.$$)"
echo "first timer, largest: $(awk '{print $2}' /tmp/late.$$ | sort -n | tail -5 | tr '\n' ' ')"
echo "second timer, largest: $(awk '{print $3}' /tmp/late.$$ | sort -n | tail -5 | tr '\n' ' ')"
echo "callback at (ms from start), largest: $(awk '{print $4}' /tmp/late.$$ | sort -n | tail -5 | tr '\n' ' ')"
awk '{b = $2 < 10 ? "<10" : ($2 < 50 ? "10-49" : ($2 < 200 ? "50-199" : ">=200")); h[b]++} END {for (k in h) print "  first " k ": " h[k]}' /tmp/late.$$
rm -f /tmp/late.$$
