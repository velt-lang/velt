#!/bin/bash
# raced_first (ns_t_update.velt) with the cross-thread wake window forced: main's std with a pause
# after the wake in both counted wakes (task and node), and the wake-order std with
# VELT_SCHED_WAKE_PAUSE. Counted runs at 4 and 8 workers.
set -u
R=$HOME/dev/velt/branches/r3420-254e
I=$R/i3487
W=$HOME/dev/velt/branches/fix-3420-254e
n=${1:-20}
prog=$R/issues/ns_t_update.velt
rm -rf "$R/std-mainp" && cp -R "$R/perf/base/std" "$R/std-mainp"
D='for (let i = 0; i < 2000; i++) {\n    m.yieldCore();\n  }\n'
perl -0pi -e "s/(function countedWake\(w: usize\): void \{\n  WAKING\.add\(1\);\n  wake\(w\);\n)/\$1  $D/" "$R/std-mainp/lang/hooks/sched_wake.velt"
perl -0pi -e "s/(function countedWake\(w: usize\): void \{\n  wk\.wakingStarts\(\);\n  wake\(w\);\n)/\$1  $D/" "$R/std-mainp/lang/hooks/sched_node_wake.velt"
echo "mainp: $(diff -r "$R/perf/base/std" "$R/std-mainp" | grep -c '^>') lines"
V=$W/target/debug/velt
for s in main mainp fix; do
  case $s in main) S=$R/perf/base/std; P=0;; mainp) S=$R/std-mainp; P=0;; fix) S=$W/std; P=2000;; esac
  VELT_STD=$S "$V" build "$prog" -o "$I/rf-$s" > "$I/brf-$s.log" 2>&1 || { echo "$s build failed"; tail -4 "$I/brf-$s.log"; continue; }
  for th in 4 8; do
    left=0; bad=0; last=""
    for i in $(seq 1 "$n"); do
      out=$(VELT_SCHED_WAKE_PAUSE=$P VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "$I/rf-$s" 2>&1); code=$?
      [ $code -ne 0 ] && bad=$((bad + 1))
      b=$(echo "$out" | grep -o "blocks=[0-9]*/[0-9]*" | tail -1)
      echo "$b" | awk -F'[=/]' '$2 != $3 {exit 1}' || { left=$((left + 1)); last=$b; }
    done
    echo "$s T$th: $left of $n leaked ($last), $bad exit!=0"
  done
done
