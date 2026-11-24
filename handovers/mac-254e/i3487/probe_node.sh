#!/bin/bash
# nodeWake probe: node_wake.velt with the fix std, and with a scratch std that gives the core away
# 2000 times between nodeWake's wake and its release. Counted runs at 4 and 8 workers.
set -u
R=$HOME/dev/velt/branches/r3420-254e
I=$R/i3487
W=$HOME/dev/velt/branches/fix-3420-254e
n=${1:-20}; prog=${2:-node_wake}
rm -rf "$R/std-npause" && cp -R "$W/std" "$R/std-npause"
perl -0pi -e 's/(function countedWake\(w: usize\): void \{\n  wk\.wakingStarts\(\);\n  wake\(w\);\n)/$1  for (let i = 0; i < 2000; i++) {\n    m.yieldCore();\n  }\n/' "$R/std-npause/lang/hooks/sched_node_wake.velt"
echo "npause: $(diff -r "$W/std" "$R/std-npause" | grep -c '^>') lines"
V=$W/target/debug/velt
for s in fix npause; do
  if [ $s = fix ]; then S=$W/std; else S=$R/std-npause; fi
  VELT_STD=$S "$V" build "$I/$prog.velt" -o "$I/np-$s" > "$I/bn-$s.log" 2>&1 || { echo "$s build failed"; tail -4 "$I/bn-$s.log"; continue; }
  for th in 4 8; do
    left=0; bad=0; last=""
    for i in $(seq 1 "$n"); do
      out=$(VELT_THREADS=$th VELT_RT_DEBUG_ALLOC=1 VELT_RC_STATS=1 "$I/np-$s" 2>&1); code=$?
      [ $code -ne 0 ] && { bad=$((bad + 1)); last=$(echo "$out" | tail -1 | cut -c1-120); }
      echo "$out" | grep -o "blocks=[0-9]*/[0-9]*" | tail -1 | awk -F'[=/]' '$2 != $3 {exit 1}' || { left=$((left + 1)); last=$(echo "$out" | grep -o "blocks=[0-9]*/[0-9]*" | tail -1); }
    done
    echo "$s T$th: $left of $n leaked, $bad exit!=0 $last"
  done
done
