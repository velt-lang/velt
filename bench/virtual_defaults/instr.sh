#!/bin/bash
# Instruction counts (cachegrind) of the loop programs, each built with two compilers, then
# the general fill entry (`pick`) against the same loop written by hand (`pick_hand`: each
# override applies its own default to a `number | undefined`), both built with HEAD.
# usage: BASE=<velt> HEAD=<velt> bench/virtual_defaults/instr.sh   (BASE may be left out)
set -e
dir=$(cd "$(dirname "$0")" && pwd)
out=${OUT:-$(mktemp -d)}
count() { # count <program> <velt> <tag>
  "$2" build --release "$dir/$1.vlt" -o "$out/$1.$3" > /dev/null
  VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no \
    --cachegrind-out-file="$out/cg.$1.$3.out" "$out/$1.$3" > /dev/null 2> "$out/vg.$1.$3.txt"
  echo "$1 $3 $(grep -E 'I *refs' "$out/vg.$1.$3.txt" | sed 's/.*refs: *//')"
}
if [ -n "$BASE" ]; then
  for p in iface if1; do
    count $p "$BASE" base
    count $p "$HEAD" head
  done
fi
count pick "$HEAD" head
count pick_hand "$HEAD" head
