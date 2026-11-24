#!/bin/bash
# #3487 break checks: each part of the change taken out, both programs counted 10 runs at 4 and 8.
set -u
R=$HOME/dev/velt/branches/r3420-254e
I=$R/i3487
W=$HOME/dev/velt/branches/fix-3420-254e
H=lang/hooks/sched_share_listed.velt
mk() { rm -rf "$R/std-$1"; cp -R "$W/std" "$R/std-$1"; }
mk b1; perl -0pi -e 's/(function waiting\(task: usize, at: usize, w: usize\): void \{\n)/$1  return;\n/' "$R/std-b1/$H"
mk b2; perl -0pi -e 's/      if \(x !== task\) \{/      if (false \&\& x !== task) {/; s/      m\.st\(at \+ 8, m\.wakerClone\(x\)\);\n/      if (x === task) {\n        m.st(at + 8, m.wakerClone(x));\n      }\n/' "$R/std-b2/$H"
mk b3; perl -0pi -e 's/(function sweep\(task: usize, h: usize\): void \{\n)/$1  return;\n/' "$R/std-b3/$H"
for b in b1 b2 b3; do echo "$b: $(diff -r "$W/std" "$R/std-$b" | grep -c '^[<>]') lines"; done
V=$W/target/debug/velt
cd "$I" || exit 1
for b in b1 b2 b3; do
  for p in ns_waiting_in_hub ns_waiting_handed; do
    VELT_STD=$R/std-$b "$V" build "$p.velt" -o "k-$p-$b" > "bk.log" 2>&1 || { echo "$b $p build"; tail -3 bk.log; continue; }
    for th in 4 8; do
      echo "$b $p T$th: $(bash par.sh "$I/k-$p-$b" $th 2 5 "k-$p-$b-$th" | head -2 | tr '\n' ' ' | cut -c1-110)"
    done
  done
done
