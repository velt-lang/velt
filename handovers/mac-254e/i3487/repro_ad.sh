#!/bin/bash
# awaited_dropped under oversubscription: one busy process per core, then <n> counted runs of the
# base and the fix interleaved at <threads> workers; counts runs that leave blocks. Kills only the
# busy processes it started, by PID.
set -u
I=$HOME/dev/velt/branches/r3420-254e/i3487
n=${1:-50}; th=${2:-8}
cores=$(sysctl -n hw.ncpu)
pids=()
for i in $(seq 1 "$cores"); do
  ( while :; do :; done ) &
  pids+=($!)
done
trap 'kill "${pids[@]}" 2>/dev/null' EXIT
lb=0; ln=0; eb=0; en=0
for i in $(seq 1 "$n"); do
  for s in base new; do
    out=$(VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "$I/ad-$s" 2>&1)
    code=$?
    left=$(echo "$out" | grep -o "blocks=[0-9]*/[0-9]*" | tail -1 | awk -F'[=/]' '$2 != $3 {print 1}')
    if [ "$s" = base ]; then
      [ -n "$left" ] && lb=$((lb + 1)); [ $code -ne 0 ] && eb=$((eb + 1))
    else
      [ -n "$left" ] && ln=$((ln + 1)); [ $code -ne 0 ] && en=$((en + 1))
    fi
  done
done
echo "T$th, $n runs each, $cores busy: base leaked $lb (exit!=0 $eb); fix leaked $ln (exit!=0 $en)"
