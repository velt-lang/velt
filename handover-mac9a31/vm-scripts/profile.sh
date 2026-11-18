#!/bin/bash
# Inside the VM: the head program rebuilt with debug info (-g), then callgrind of it reading the
# 1M lines; the top functions by self instructions.
set -u
B=$VELT_ROOT/branches
OUT=$HOME/stdin-out
export VELT_BUILD_CACHE=off
VELT_STD=$B/stdin-2196/std VELT_RT_LIB=$HOME/tgt-stdin-head/release/libvelt_rt.a \
  "$HOME/tgt-stdin-head/release/velt" build "$B/vm-stdin/lines.velt" --release -g -o "$OUT/lines-head-g" || exit 1
valgrind --tool=callgrind --callgrind-out-file="$OUT/cg.out" "$OUT/lines-head-g" < "$OUT/lines.txt" > /dev/null 2>&1
callgrind_annotate "$OUT/cg.out" 2>/dev/null | sed -n '/PROGRAM TOTALS/,$p' | head -30
