#!/usr/bin/env bash
# Instruction counts (cachegrind) of the `any` operator and method loops (#924): hot.ts, and
# eq.ts (`==` of primitives), eq_num.ts (of numbers), eq_null.ts (`x == null`) and
# cast_member.ts / cast_member_boxonce.ts (`(c as V).f` on a union) by default.
#   bench/any_ops/count.sh <release velt> <std dir> <release libvelt_rt.a> [program.ts...]
set -euo pipefail
velt=$1 std=$2 rt=$3
shift 3
dir=$(cd "$(dirname "$0")" && pwd)
[ $# -gt 0 ] || set -- "$dir/hot.ts" "$dir/eq.ts" "$dir/eq_num.ts" "$dir/eq_null.ts" "$dir/cast_member.ts" "$dir/cast_member_boxonce.ts"
tmp=$(mktemp -d)
for p in "$@"; do
  VELT_STD="$std" VELT_RT_LIB="$rt" "$velt" build "$p" -o "$tmp/prog" --release
  n=$(valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$tmp/prog" 2>&1 >/dev/null |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,)
  printf '%s\t%s\n' "$(basename "$p")" "$n"
done
rm -rf "$tmp"
