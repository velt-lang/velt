#!/bin/bash
# The `any` comparison matrix against Node (#924): 26 values x 26 values x 4 operators
# (=== == !== !=), written directly, in a closure, through fields, elements and calls.
# usage: bench/any_matrix/run.sh <velt binary> <work dir>
# Prints the number of comparisons and whether debug (checking allocator) and release match Node.
set -e
velt=$1; work=$2
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$work" && cd "$work"
node "$here/gen.js"
node matrix.ts > node.out
VELT_RT_DEBUG_ALLOC=1 "$velt" run matrix.vlt > debug.out
"$velt" run --release matrix.vlt 2> /dev/null > release.out
for m in debug release; do
  rows=$(grep -c ': ' node.out)
  same=$(diff <(grep ': ' node.out) <(grep ': ' $m.out) | grep -c '^<' || true)
  echo "$m: $((rows - same)) of $rows rows match Node"
done
