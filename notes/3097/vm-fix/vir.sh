#!/bin/bash
# VIR (`--emit vir`, release) of bench NAME, base std and head std, in the VM; then the calls of
# function FN (a substring of its VIR name) in each, side by side.
#   orb -m velt-prof bash notes/3097/vm-fix/vir.sh race_select '_V4main$poll'
set -u
OUT=$HOME/b3097fix
WT=/Users/andii/dev/velt/branches/cross-wake
V=$OUT/tgt/release/velt
name=$1; fn=$2
src=$WT/bench/async/$name.velt
for s in base head; do
  std=$OUT/base-std; [ $s = head ] && std=${HEAD_STD:-$WT/std}
  VELT_STD=$std "$V" build --release --backend llvm --emit vir "$src" >"$OUT/$name-$s.vir" 2>&1
  awk -v fn="$fn" '
    /^fn#/ { on = index($0, fn " ") > 0 || index($0, fn "(") > 0; if (on) print "  def: " substr($0, 1, 120); next }
    on && /call fn#/ { sub(/.*call fn#[0-9]+ /, ""); sub(/\(.*/, ""); print }' "$OUT/$name-$s.vir" \
    | sort | uniq -c | sort -rn >"$OUT/vir-calls-$s.txt"
  echo "== $s: $(wc -l <"$OUT/$name-$s.vir") lines"
done
diff "$OUT/vir-calls-base.txt" "$OUT/vir-calls-head.txt"
