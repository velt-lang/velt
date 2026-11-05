#!/usr/bin/env bash
# What `console.log` and `util.inspect` of typed values cost (docs/internals/design/inspect-shapes.md):
# cachegrind instructions of `velt check` and `velt build --release`, children counted, and of the
# built programs, with one compiler against another. Linux only (valgrind).
#
#   bench/inspect/rows.sh WORK NAME=TREE=VELT ...
#
# Each side is NAME=TREE=VELT: a label, its source tree (for std) and its release `velt` binary.
# Prints `row<TAB>side<TAB>instructions`, one line per row, side and run, to stdout. Every run has
# an empty VELT_HOME, the build cache and stamp off and one thread, so that nothing a run leaves
# behind counts in the next one, and the sides take turns (a compile row repeats within about 0.03%).
set -euo pipefail
[ $# -ge 2 ] || { echo "usage: bench/inspect/rows.sh WORK NAME=TREE=VELT ..." >&2; exit 2; }
WORK=$1; shift
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
HERE=$(cd "$(dirname "$0")" && pwd)
RUNS=${RUNS:-2}

# Instructions of a command and its children: the sum of cachegrind's totals.
count() {
  local dir out n
  dir=$(mktemp -d "$WORK/cg.XXXXXX")
  valgrind --tool=cachegrind --cache-sim=no --trace-children=yes \
    --cachegrind-out-file="$dir/cg.%p" "$@" > /dev/null 2> "$dir/stderr" || {
    echo "failed: $*" >&2; tail -5 "$dir/stderr" >&2; exit 1
  }
  n=$(cat "$dir"/cg.* | awk '/^summary:/ { s += $2 } END { printf "%d", s }')
  rm -rf "$dir"
  printf '%s' "$n"
}

compiler() { # side tree velt args...
  local tree=$2 velt=$3 home
  shift 3
  home=$(mktemp -d "$WORK/home.XXXXXX")
  (
    export VELT_HOME="$home" VELT_STD="$tree/std" VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off VELT_THREADS=1
    count "$velt" "$@"
  )
  rm -rf "$home"
}

row() { # name args... ; for each side, in turn
  local name=$1 run side label tree velt
  shift
  for run in $(seq 1 "$RUNS"); do
    for side in "${SIDES[@]}"; do
      IFS='=' read -r label tree velt <<< "$side"
      printf '%s\t%s\t%s\n' "$name" "$label" "$(compiler "$label" "$tree" "$velt" "$@")"
    done
  done
}

SIDES=("$@")

COMPILE=(log_string log_number log_object log_array log_class log_map log_types)
row "check hello" check "$HERE/hello.velt"
for p in "${COMPILE[@]}"; do
  row "check $p" check "$HERE/$p.velt"
done
row "compile/check_all_std" check --std
row "check struct, no print" check "$HERE/struct_no_print.velt"

for p in hello "${COMPILE[@]}"; do
  for side in "${SIDES[@]}"; do
    IFS='=' read -r label tree velt <<< "$side"
    exe="$WORK/$label-$p"
    home=$(mktemp -d "$WORK/home.XXXXXX")
    n=$(
      export VELT_HOME="$home" VELT_STD="$tree/std" VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off VELT_THREADS=1
      count "$velt" build --release "$HERE/$p.velt" -o "$exe"
    )
    rm -rf "$home"
    printf 'build --release %s\t%s\t%s\n' "$p" "$label" "$n"
  done
done

for p in run_string run_number run_object run_array run_class run_map; do
  for side in "${SIDES[@]}"; do
    IFS='=' read -r label tree velt <<< "$side"
    exe="$WORK/$label-$p"
    home=$(mktemp -d "$WORK/home.XXXXXX")
    (
      export VELT_HOME="$home" VELT_STD="$tree/std" VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off
      "$velt" build --release "$HERE/$p.velt" -o "$exe" > /dev/null
    )
    rm -rf "$home"
    printf 'run %s\t%s\t%s\n' "$p" "$label" "$(count "$exe")"
  done
done
