#!/bin/bash
# awaited_dropped with VELT_SCHED_WAKE_PAUSE: the fix, and a break std with the old order
# (wake, pause, release). Counted runs at 4 and 8 workers.
set -u
R=$HOME/dev/velt/branches/r3420-254e
I=$R/i3487
W=$HOME/dev/velt/branches/fix-3420-254e
n=${1:-20}; p=${2:-2000}; prog=${3:-awaited_dropped}
rm -rf "$R/std-pbreak" && cp -R "$W/std" "$R/std-pbreak"
perl -0pi -e 's/  lt\.releaseAway\(w\);\n\}/  wake(w);\n  afterWake();\n  release(w);\n}/' "$R/std-pbreak/lang/hooks/sched_wake.velt"
echo "break: $(diff -r "$W/std" "$R/std-pbreak" | grep -c '^[<>]') lines"
V=$W/target/debug/velt
for s in fix pbreak; do
  if [ $s = fix ]; then S=$W/std; else S=$R/std-pbreak; fi
  VELT_STD=$S "$V" build "$I/$prog.velt" -o "$I/pa-$s" > "$I/bp-$s.log" 2>&1 || { echo "$s build failed"; tail -4 "$I/bp-$s.log"; continue; }
  for th in 4 8; do
    left=0; bad=0
    for i in $(seq 1 "$n"); do
      out=$(VELT_SCHED_WAKE_PAUSE=$p VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "$I/pa-$s" 2>&1); code=$?
      [ $code -ne 0 ] && bad=$((bad + 1))
      echo "$out" | grep -o "blocks=[0-9]*/[0-9]*" | tail -1 | awk -F'[=/]' '$2 != $3 {exit 1}' || left=$((left + 1))
    done
    echo "$s T$th: $left of $n leaked, $bad exit!=0"
  done
done
