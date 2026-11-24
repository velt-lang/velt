#!/bin/bash
# #3487 review fixes, break checks: bt = transfer without leaveWaiting, bo = offList without the
# shared node's own hub. Each program counted 10 runs at 4 and 8 workers, against the fix.
set -u
R=$HOME/dev/velt/branches/r3420-254e
I=$R/i3487
W=$HOME/dev/velt/branches/fix-3420-254e
mk() { rm -rf "$R/std-$1"; cp -R "$W/std" "$R/std-$1"; }
mk bt; perl -0pi -e 's/    ls\.leaveWaiting\(n\);\n/    \/\/ broken\n/' "$R/std-bt/lang/hooks/sched_node_transfer.velt"
mk bo; perl -0pi -e 's/    unlisted\(task, n, fl\);\n  \}\n/    \/\/ broken\n  }\n/' "$R/std-bo/lang/hooks/sched_share_listed.velt"
for b in bt bo; do echo "$b: $(diff -r "$W/std" "$R/std-$b" | grep -c '^[<>]') lines"; done
V=$W/target/debug/velt
cd "$I" || exit 1
for b in fix bt bo; do
  if [ $b = fix ]; then S=$W/std; else S=$R/std-$b; fi
  for p in ns_waiting_moved ns_waiting_shared_off; do
    VELT_STD=$S "$V" build "$p.velt" -o "k-$p-$b" > bk.log 2>&1 || { echo "$b $p build"; tail -3 bk.log; continue; }
    for th in 4 8; do
      echo "$b $p T$th: $(bash par.sh "$I/k-$p-$b" $th 2 5 "k-$p-$b-$th" | head -2 | tr '\n' ' ' | cut -c1-190)"
    done
  done
done
