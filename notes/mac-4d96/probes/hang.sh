#!/bin/bash
# Looks for the hang of a handed-on probe under load: J loops at once, N runs each, of
# <exe> at <threads> workers; a run still alive after 25 s is sampled (macOS `sample`) and killed.
#   bash hang.sh <exe> <threads> [jobs=8] [runs=25]
cd "$(dirname "$0")" || exit 1
exe=$1; th=$2; jobs=${3:-8}; runs=${4:-25}
mkdir -p hang
one() {
  local j=$1 hung=0 bad=0
  for i in $(seq 1 "$runs"); do
    VELT_SCHED_DEBUG=${SDBG:-0} VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "$exe" > "hang/out-$j.txt" 2> "hang/err-$j.txt" &
    pid=$!
    for t in $(seq 1 250); do
      kill -0 $pid 2>/dev/null || break
      sleep 0.1
    done
    if kill -0 $pid 2>/dev/null; then
      hung=$((hung + 1))
      cp "hang/err-$j.txt" "hang/hung-err-$j-$i.txt"; sample $pid 2 -file "hang/sample-$j-$i.txt" > /dev/null 2>&1
      kill -9 $pid 2>/dev/null
      wait $pid 2>/dev/null
    else
      wait $pid || bad=$((bad + 1))
      grep -q "fired 1000" "hang/out-$j.txt" || bad=$((bad + 1))
    fi
  done
  echo "$hung $bad" > "hang/count-$j"
}
for j in $(seq 1 "$jobs"); do one "$j" & done
wait
h=0; b=0
for j in $(seq 1 "$jobs"); do read -r x y < "hang/count-$j"; h=$((h + x)); b=$((b + y)); done
echo "$(basename "$exe") T$th: $h hung, $b bad of $((jobs * runs))"
ls hang | grep -c sample
