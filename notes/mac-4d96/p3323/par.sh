#!/bin/bash
# A built probe run J at a time, N rounds, at T workers, counted: how many runs leak.
#   bash par.sh <exe> <threads> <jobs> <rounds>
cd "$(dirname "$0")" || exit 1
exe=$1; th=$2; jobs=$3; rounds=$4
mkdir -p par
one() {
  local j=$1
  for i in $(seq "$rounds"); do
    VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "$exe" > par/o$j.txt 2> par/e$j.txt
    local code=$?
    local b=$(grep -o 'blocks=[0-9]*/[0-9]*' par/e$j.txt | tail -1 | awk -F'[=/]' '{print ($2==$3)?"bal":"LEFT"}')
    echo "code=$code $b $(grep -o 'ICE: [a-z ]*' par/e$j.txt | head -1 | cut -c1-50)"
  done
}
for j in $(seq "$jobs"); do one "$j" > par/r$j.txt & done
wait
cat par/r*.txt | sort | uniq -c
