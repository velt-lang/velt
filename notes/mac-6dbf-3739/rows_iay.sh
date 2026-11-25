#!/bin/bash
# iay2 (the coordinator's inline_all_yields: 2 yields per child) and the bench row, PR1 vs 18cc vs this std.
set -u
S="/private/tmp/claude-501/-Users-andii-dev-velt-main/6dbf5d3c-1862-4ea4-a154-094b5a8b6b53/scratchpad"
W="$HOME/dev/velt/branches/root-turns-3687-r1"
T="$HOME/dev/velt/branches/mac6dbf-target"
V="$T/debug/velt"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.sdk VELT_RT_LIB="$T/release/libvelt_rt.a"
A="$S/abx"; rm -rf "$A" && mkdir -p "$A"
instr() { VELT_THREADS=1 /usr/bin/time -l "$@" 2>&1 >/dev/null | awk '/instructions retired/ { print $1 }'; }
median() { sort -n | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }'; }
cd "$W" || exit 1
for p in "$S/rows3687/iay2.velt" bench/async/inline_all_yields.velt ${EXTRA:-}; do
  n=$(basename "$p" .velt)
  VELT_STD="$S/pr1/std" "$S/c3687r/velt-base" build --release "$p" -o "$A/$n-pr1" > /dev/null 2>&1 || echo "BUILD FAILED pr1"
  VELT_STD="$S/std18cc/std" "$V" build --release "$p" -o "$A/$n-old" > /dev/null 2>&1 || echo "BUILD FAILED old"
  VELT_STD="$W/std" "$V" build --release "$p" -o "$A/$n-new" > /dev/null 2>&1 || echo "BUILD FAILED new"
  a=""; b=""; c=""
  for k in 1 2 3 4 5; do a="$a $(instr "$A/$n-pr1")"; b="$b $(instr "$A/$n-old")"; c="$c $(instr "$A/$n-new")"; done
  am=$(echo $a | tr ' ' '\n' | median); bm=$(echo $b | tr ' ' '\n' | median); cm=$(echo $c | tr ' ' '\n' | median)
  awk -v p="$n" -v a="$am" -v b="$bm" -v c="$cm" 'BEGIN { printf "%-22s pr1 %11d 18cc %11d (%+.2f%%) new %11d (%+.2f%% vs pr1, %+.2f%% vs 18cc)\n", p, a, b, (b - a) * 100 / a, c, (c - a) * 100 / a, (c - b) * 100 / b }'
done
rm -rf "$A"
