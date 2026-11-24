#!/bin/bash
# A built program run J at a time, N rounds each, at T workers, counted: exit codes, balanced or
# leaking block counts, and every leaking run's stats line.
#   bash par.sh <exe> <threads> <jobs> <rounds> <tag>
cd "$(dirname "$0")" || exit 1
exe=$1; th=$2; jobs=$3; rounds=$4; tag=$5
mkdir -p "par-$tag"
one() {
  local j=$1
  for i in $(seq "$rounds"); do
    VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "$exe" > "par-$tag/o$j.txt" 2> "par-$tag/e$j.txt"
    local code=$?
    local b
    b=$(grep -o 'blocks=[0-9]*/[0-9]*' "par-$tag/e$j.txt" | tail -1)
    local ok
    ok=$(echo "$b" | awk -F'[=/]' '{print ($2==$3)?"bal":"LEFT"}')
    echo "code=$code $ok $(head -c 60 "par-$tag/o$j.txt" | tr '\n' ' ')"
    if [ "$ok" != bal ] || [ $code -ne 0 ]; then echo "$j/$i $b $(cat "par-$tag/e$j.txt" | tr '\n' ' ' | cut -c1-300)" >> "par-$tag/left.txt"; fi
  done
}
: > "par-$tag/left.txt"
for j in $(seq "$jobs"); do one "$j" > "par-$tag/r$j.txt" & done
wait
cat "par-$tag"/r*.txt | sort | uniq -c
head -5 "par-$tag/left.txt"
