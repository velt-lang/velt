#!/bin/bash
# Instructions of each row of bench/counted_field_args.velt, built by two compilers, one row per
# program. Each compiler is a release build with its own runtime and standard library: give the
# directory of a checkout whose target/release has both.
#
# usage: WORK=<dir> bench/counted_field_args.sh <base checkout> <head checkout> [<target dir base> <target dir head>]
set -eu
base=$1
head=$2
tb=${3:-$base/target}
th=${4:-$head/target}
: "${WORK:?set WORK to a work directory}"
mkdir -p "$WORK"
here=$(cd "$(dirname "$0")" && pwd)

count() {
  VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$1" 2>&1 >/dev/null |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,
}

build() { # <checkout> <target dir> <source> <exe>
  VELT_STD="$1/std" VELT_RT_LIB="$2/release/libvelt_rt.a" "$2/release/velt" build --release --backend llvm "$3" -o "$4" >/dev/null
}

rows=$(sed -n '/^function all() {/,/^}/s/.*console.log(\([A-Za-z]*\)()).*/\1/p' "$here/counted_field_args.velt")
for row in $rows; do
  src="$WORK/row_$row.velt"
  sed "/^function all() {/,/^}/{/console.log/{/console.log($row())/!d}}" "$here/counted_field_args.velt" >"$src"
  build "$base" "$tb" "$src" "$WORK/$row.base"
  build "$head" "$th" "$src" "$WORK/$row.head"
  b=$(count "$WORK/$row.base")
  h=$(count "$WORK/$row.head")
  awk -v n="$row" -v b="$b" -v h="$h" 'BEGIN { printf "%s base=%d head=%d change=%+.2f%% per_call=%+.1f\n", n, b, h, (h - b) * 100 / b, (h - b) / 300000 }'
done
