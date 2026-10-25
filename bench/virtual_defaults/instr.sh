#!/bin/bash
# Instruction counts (cachegrind) of the interface loop programs, built with two compilers.
# usage: BASE=<velt> HEAD=<velt> bench/virtual_defaults/instr.sh
set -e
dir=$(cd "$(dirname "$0")" && pwd)
out=${OUT:-$(mktemp -d)}
for p in iface if1; do
  for v in base head; do
    velt=$BASE; [ "$v" = head ] && velt=$HEAD
    "$velt" build --release "$dir/$p.vlt" -o "$out/$p.$v" > /dev/null
    VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no \
      --cachegrind-out-file="$out/cg.$p.$v.out" "$out/$p.$v" > /dev/null 2> "$out/vg.$p.$v.txt"
    echo "$p $v $(grep -E 'I *refs' "$out/vg.$p.$v.txt" | sed 's/.*refs: *//')"
  done
done
