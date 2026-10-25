#!/usr/bin/env bash
# Instruction count (cachegrind) of the `any` operator and method loop (#924):
#   bench/any_ops/count.sh <release velt> <std dir> <release libvelt_rt.a>
set -euo pipefail
tmp=$(mktemp -d)
VELT_STD="$2" VELT_RT_LIB="$3" "$1" build "$(dirname "$0")/hot.ts" -o "$tmp/hot" --release
"$tmp/hot"
valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$tmp/hot" 2>&1 | grep -E "I +refs"
rm -rf "$tmp"
