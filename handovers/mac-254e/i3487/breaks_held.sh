#!/bin/bash
# #3498 break checks: h1 hold() counts nothing; h2 a free doesn't uncount; h3 a hand-over doesn't
# uncount. The four programs counted 5 runs at 4 and 8 workers each.
set -u
R=$HOME/dev/velt/branches/r3420-254e
I=$R/i3487
W=$HOME/dev/velt/branches/fix-3420-254e
H=lang/hooks
mk() { rm -rf "$R/std-$1"; cp -R "$W/std" "$R/std-$1"; }
mk h1; perl -0pi -e 's/(export function hold\(task: usize, n: usize\): void \{\n)/$1  return;\n/' "$R/std-h1/$H/sched_held.velt"
mk h2; perl -0pi -e 's/      \/\/ Before its reference to its task goes \(sched_held\.velt\)\.\n      hd\.freed\(task, n\);\n/      \/\/ broken\n/' "$R/std-h2/$H/sched_node.velt"
mk h3; perl -0pi -e 's/  hd\.unhold\(m\.ld\(n \+ nd\.N_TASK\), n\);\n/  \/\/ broken\n/' "$R/std-h3/$H/sched_node_transfer.velt"
for b in h1 h2 h3; do echo "$b: $(diff -r "$W/std" "$R/std-$b" | grep -c '^[<>]') lines"; done
V=$W/target/debug/velt
cd "$I" || exit 1
for b in h1 h2 h3; do
  for p in held_plain held_handed held_kept two_roots; do
    VELT_STD=$R/std-$b "$V" build "$p.velt" -o "kh-$p-$b" > bkh.log 2>&1 || { echo "$b $p build"; tail -3 bkh.log; continue; }
    for th in 4 8; do
      echo "$b $p T$th: $(bash par.sh "$I/kh-$p-$b" $th 2 5 "kh-$p-$b-$th" | head -2 | tr '\n' ' ' | cut -c1-150)"
    done
  done
done
