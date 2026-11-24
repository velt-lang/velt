#!/bin/bash
# awaited_dropped with the window forced open: scratch stds that give the core away 2000 times
# right after a wake by value wakes the task (base: before the release; fix: inside releaseAway,
# after the wake, before the lock goes). Counted runs at 4 and 8 workers.
set -u
R=$HOME/dev/velt/branches/r3420-254e
I=$R/i3487
W=$HOME/dev/velt/branches/fix-3420-254e
n=${1:-20}
rm -rf "$R/std-dbase" "$R/std-dfix"
cp -R "$R/perf/base/std" "$R/std-dbase"
cp -R "$W/std" "$R/std-dfix"
D='for (let i = 0; i < 2000; i++) {\n    m.yieldCore();\n  }\n'
perl -0pi -e "s/(function countedWake\(w: usize\): void \{\n  WAKING\.add\(1\);\n  wake\(w\);\n)/\$1  $D/" "$R/std-dbase/lang/hooks/sched_wake.velt"
perl -0pi -e "s/(  if \(!last\) \{\n    wk\.wake\(k\);\n)/\$1    $D/" "$R/std-dfix/lang/hooks/sched_task_lost.velt"
echo "dbase: $(diff -r "$R/perf/base/std" "$R/std-dbase" | grep -c '^>') lines, dfix: $(diff -r "$W/std" "$R/std-dfix" | grep -c '^>') lines"
V=$W/target/debug/velt
for s in dbase dfix; do
  VELT_STD=$R/std-$s "$V" build "$I/awaited_dropped.velt" -o "$I/ad-$s" > "$I/b-$s.log" 2>&1 || { echo "$s build failed"; tail -4 "$I/b-$s.log"; continue; }
  for th in 4 8; do
    left=0; bad=0
    for i in $(seq 1 "$n"); do
      out=$(VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "$I/ad-$s" 2>&1); code=$?
      [ $code -ne 0 ] && bad=$((bad + 1))
      echo "$out" | grep -o "blocks=[0-9]*/[0-9]*" | tail -1 | awk -F'[=/]' '$2 != $3 {exit 1}' || left=$((left + 1))
    done
    echo "$s T$th: $left of $n leaked, $bad exit!=0"
  done
done
