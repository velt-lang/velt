#!/bin/bash
# `velt build --release --timings` of APP (examples/apps/<app>) with main's and the fix's velt:
# the deterministic sizes (IR per unit, imports) to compare.
#   orb -m velt-prof bash notes/3097/vm-fix/irsize.sh actors
set -u
OUT=$HOME/b3097fix
WT=/Users/andii/dev/velt/branches/units-inline-import
app=$1
export VELT_STD=$WT/std VELT_RT_LIB=$OUT/tgt/release/libvelt_rt.a VELT_BUILD_CACHE=off
cd "$WT/examples/apps/$app" || exit 1
for s in main units; do
  rm -f "$OUT/ir-$app-$s"
  "$OUT/tgt-$s/release/velt" build --release --timings -o "$OUT/ir-$app-$s" >"$OUT/ir-$app-$s.txt" 2>&1
  echo "== $s"
  grep -i -E "unit|ir|line|import|llvm|clang|codegen" "$OUT/ir-$app-$s.txt" | head -30
done
