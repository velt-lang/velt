#!/bin/bash
# #3487 break checks for `unlisted`: b2a gives the waiting promise's reference back but leaves it
# listed; b2b takes it off the list but leaves its entry uncounted. ns_waiting_handed, 10 runs at 4/8.
set -u
R=$HOME/dev/velt/branches/r3420-254e
I=$R/i3487
W=$HOME/dev/velt/branches/fix-3420-254e
H=lang/hooks/sched_share_listed.velt
mk() { rm -rf "$R/std-$1"; cp -R "$W/std" "$R/std-$1"; }
mk b2a; perl -0pi -e 's/        if \(\(xf & \(lt\.LISTED \| nd\.FINISHED\)\) === lt\.LISTED\) \{/        if (false \&\& (xf \& (lt.LISTED | nd.FINISHED)) === lt.LISTED) {/' "$R/std-b2a/$H"
mk b2b; perl -0pi -e 's/      m\.st\(at \+ 8, m\.wakerClone\(x\)\);\n      if \(x !== task\) \{/      if (x === task) {\n        m.st(at + 8, m.wakerClone(x));\n      }\n      if (x !== task) {/' "$R/std-b2b/$H"
for b in b2a b2b; do echo "$b: $(diff -r "$W/std" "$R/std-$b" | grep -c '^[<>]') lines"; done
V=$W/target/debug/velt
cd "$I" || exit 1
for b in b2a b2b; do
  for p in ns_waiting_handed; do
    VELT_STD=$R/std-$b "$V" build "$p.velt" -o "k-$p-$b" > bk.log 2>&1 || { echo "$b $p build"; tail -3 bk.log; continue; }
    for th in 4 8; do
      echo "$b $p T$th: $(bash par.sh "$I/k-$p-$b" $th 2 5 "k-$p-$b-$th" | head -2 | tr '\n' ' ' | cut -c1-200)"
    done
  done
done
