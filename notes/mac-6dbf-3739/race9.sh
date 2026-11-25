#!/bin/bash
# race_yield: ec4258839's std vs this std, same compiler, 9 interleaved runs each, min and median.
set -u
S="/private/tmp/claude-501/-Users-andii-dev-velt-main/6dbf5d3c-1862-4ea4-a154-094b5a8b6b53/scratchpad"
W="$HOME/dev/velt/branches/root-turns-3687-r1"
T="$HOME/dev/velt/branches/mac6dbf-target"
V="$T/debug/velt"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk VELT_RT_LIB="$T/release/libvelt_rt.a"
A="$S/ab9"; rm -rf "$A" && mkdir -p "$A"
cd "$W" || exit 1
for p in bench/async/race_yield.velt bench/async/all_small_stored.velt; do
  n=$(basename "$p" .velt)
  VELT_STD="$S/stdec/std" "$V" build --release "$p" -o "$A/$n-old" > /dev/null 2>&1 || echo "BUILD FAILED old"
  VELT_STD="$W/std" "$V" build --release "$p" -o "$A/$n-new" > /dev/null 2>&1 || echo "BUILD FAILED new"
  a=""; b=""
  for k in 1 2 3 4 5 6 7 8 9; do
    a="$a $(VELT_THREADS=1 /usr/bin/time -l "$A/$n-old" 2>&1 >/dev/null | awk '/instructions retired/ { print $1 }')"
    b="$b $(VELT_THREADS=1 /usr/bin/time -l "$A/$n-new" 2>&1 >/dev/null | awk '/instructions retired/ { print $1 }')"
  done
  echo "$n old:$a"
  echo "$n new:$b"
  echo "$a" | tr ' ' '\n' | grep . | sort -n | awk -v n="$n" '{ v[NR] = $1 } END { printf "%s old min %d med %d\n", n, v[1], v[5] }'
  echo "$b" | tr ' ' '\n' | grep . | sort -n | awk -v n="$n" '{ v[NR] = $1 } END { printf "%s new min %d med %d\n", n, v[1], v[5] }'
done
rm -rf "$A"
