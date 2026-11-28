#!/usr/bin/env bash
# Build cost of programs with a warm build cache, one compiler against another: each side builds
# the program once in an empty VELT_HOME (making std's prebuilt hook objects), then the
# cachegrind instructions (children counted) of a second `velt check`, `velt build` and
# `velt build --release` of it in that home, with one thread. Linux only (valgrind).
#
#   bench/inspect/build_warm.sh WORK NAME=TREE=VELT ... -- PROGRAM.velt ...
set -euo pipefail
WORK=$1; shift
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
SIDES=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do SIDES+=("$1"); shift; done
shift
count() {
  local dir n
  dir=$(mktemp -d "$WORK/cg.XXXXXX")
  valgrind --tool=cachegrind --cache-sim=no --trace-children=yes \
    --cachegrind-out-file="$dir/cg.%p" "$@" > /dev/null 2> "$dir/stderr" || {
    echo "failed: $*" >&2; tail -5 "$dir/stderr" >&2; exit 1
  }
  n=$(cat "$dir"/cg.* | awk '/^summary:/ { s += $2 } END { printf "%d", s }')
  rm -rf "$dir"
  printf '%s' "$n"
}
for p in "$@"; do
  name=$(basename "$p" .velt)
  for side in "${SIDES[@]}"; do
    IFS='=' read -r label tree velt <<< "$side"
    home=$(mktemp -d "$WORK/home.XXXXXX")
    export VELT_HOME="$home" VELT_STD="$tree/std" VELT_BUILD_STAMP=off VELT_THREADS=1
    "$velt" build "$p" -o "$WORK/warm" > /dev/null
    "$velt" build --release "$p" -o "$WORK/warm" > /dev/null
    printf 'warm check %s\t%s\t%s\n' "$name" "$label" "$(count "$velt" check "$p")"
    printf 'warm debug %s\t%s\t%s\n' "$name" "$label" "$(count "$velt" build "$p" -o "$WORK/x")"
    printf 'warm release %s\t%s\t%s\n' "$name" "$label" "$(count "$velt" build --release "$p" -o "$WORK/x")"
    rm -rf "$home" "$WORK/x" "$WORK/warm"
    unset VELT_HOME VELT_STD
  done
done
