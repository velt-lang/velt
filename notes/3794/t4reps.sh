#!/bin/bash
# T4 cachegrind counts of one row, base and head interleaved, N runs each (binaries from vmrows.sh).
OUT=$HOME/s13; row=$1; n=${2:-7}
for i in $(seq 1 $n); do
  for side in base head; do
    c=$(VELT_THREADS=4 timeout 900 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$OUT/bin/$row-$side" 2>&1 > /dev/null | awk '/I *refs:/ {gsub(",", "", $NF); print $NF}')
    echo "$side $c"
  done
done
