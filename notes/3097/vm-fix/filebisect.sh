#!/bin/bash
# Which std file of the stack alone moves bench NAME's count at T=1: base std with each changed file
# taken from the head std HEAD (default 5cd66ec71's export), one at a time; then with them all.
#   orb -m velt-prof bash notes/3097/vm-fix/filebisect.sh race_select [head std dir]
set -u
OUT=$HOME/b3097fix
WT=/Users/andii/dev/velt/branches/cross-wake
V=$OUT/tgt/release/velt
RT=$OUT/tgt/release/libvelt_rt.a
name=$1; HEAD=${2:-$OUT/std-5cd66ec71}
src=$WT/bench/async/$name.velt
count() {
  VELT_STD=$1 VELT_RT_LIB=$RT "$V" build --release --backend llvm "$src" -o "$OUT/fb-$name" >"$OUT/fb.build" 2>&1 \
    || { echo "build failed: $(tail -2 "$OUT/fb.build")"; return; }
  VELT_THREADS=1 VELT_RESCUE=0 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
    "$OUT/fb-$name" 2>&1 >/dev/null | awk '/I *refs:/ {gsub(",", "", $NF); print $NF}'
}
base=$(count "$OUT/base-std")
echo "base: $base"
files=$(cd "$HEAD" && diff -rq "$OUT/base-std" . 2>/dev/null | awk '/differ/ {print $4} /^Only in \./ {print}' )
files=$(diff -rq "$OUT/base-std" "$HEAD" | sed -n 's|^Files .* and '"$HEAD"'/\(.*\) differ$|\1|p; s|^Only in '"$HEAD"'/\?\(.*\): \(.*\)$|\1/\2|p' | sed 's|^/||')
for f in $files; do
  mix=$OUT/fb-std
  rm -rf "$mix" && cp -r "$OUT/base-std" "$mix"
  mkdir -p "$(dirname "$mix/$f")" && cp "$HEAD/$f" "$mix/$f"
  c=$(count "$mix")
  awk -v f="$f" -v c="$c" -v b="$base" 'BEGIN { if (c ~ /^[0-9]+$/) printf "%-48s %12s %+7.2f%%\n", f, c, (c - b) * 100 / b; else printf "%-48s %s\n", f, c }'
done
echo "all: $(count "$HEAD")"
rm -rf "$OUT/fb-std"
