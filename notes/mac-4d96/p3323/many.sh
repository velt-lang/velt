#!/bin/bash
# One built probe, N runs at T workers (counted, checking allocator), outcomes tallied.
#   bash many.sh <exe> <threads> <runs>
cd "$(dirname "$0")" || exit 1
exe=$1; th=$2; n=$3
for i in $(seq "$n"); do
  VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "$exe" > many.out 2> many.err &
  pid=$!; ( sleep 30; kill -9 $pid 2>/dev/null ) & killer=$!
  wait $pid 2>/dev/null; code=$?; kill $killer 2>/dev/null; wait $killer 2>/dev/null
  b=$(grep -o 'blocks=[0-9]*/[0-9]*' many.err | tail -1)
  echo "code=$code $(echo "$b" | awk -F'[=/]' '{print ($2==$3)?"bal":"LEFT " $2-$3}') $(grep -o 'ICE: [a-z A-Z-]*' many.err | head -1 | cut -c1-60)"
done | sort | uniq -c
