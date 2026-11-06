#!/bin/bash
# buildcost.sh <velt> <dir> <main.vlt>: initial budget, statements charged, IR bytes, compiler
# CPU seconds and instructions of a release build (env passes through: VELT_INLINE_BUDGET).
V=$1; D=$2; M=$3
cd "$D"
out=$(mktemp -d)
VELT_INLINE_TRACE=1 VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off "$V" build --release "$M" -o $out/a 2> $out/trace
budget=$(grep -m1 "^INL" $out/trace | sed -E 's/.*budget=([0-9]+).*/\1/')
charged=$(grep "^INL" $out/trace | grep charged | awk '{s+=$NF} END{print s+0}')
ir=$(VELT_BUILD_CACHE=off "$V" build --release --emit llvm "$M" 2>/dev/null | wc -c | tr -d " ")
cost=$( { /usr/bin/time -l sh -c 'VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off "$0" build --release "$1" -o "$2" >/dev/null 2>&1' "$V" "$M" $out/b; } 2>&1 | awk '/real/{u=$3; s=$5} /instructions retired/{i=$1} END{printf "cpu=%.2fs instr=%.2fG", u+s, i/1e9}')
echo "budget=$budget charged=$charged ir=$ir $cost"
rm -rf $out
