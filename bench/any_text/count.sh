#!/usr/bin/env bash
# Instruction counts (cachegrind) of `String(v)` and templates of `any` values (#924, #925):
# objects.ts (object literals), classes.ts (class instances), maps.ts (`Map`s) and mixed.ts
# by default.
#   bench/any_text/count.sh <release velt> <std dir> <release libvelt_rt.a> [program.ts...]
set -euo pipefail
velt=$1 std=$2 rt=$3
shift 3
dir=$(cd "$(dirname "$0")" && pwd)
[ $# -gt 0 ] || set -- "$dir/objects.ts" "$dir/classes.ts" "$dir/maps.ts" "$dir/mixed.ts"
tmp=$(mktemp -d)
for p in "$@"; do
  VELT_STD="$std" VELT_RT_LIB="$rt" "$velt" build "$p" -o "$tmp/prog" --release
  n=$(valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$tmp/prog" 2>&1 >/dev/null |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,)
  printf '%s\t%s\n' "$(basename "$p")" "$n"
done
rm -rf "$tmp"
