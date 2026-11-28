#!/usr/bin/env bash
# Build cost of programs with one compiler against another: cachegrind instructions (children
# counted) of `velt check`, `velt build` and `velt build --release`, each with an empty VELT_HOME,
# the build cache on (so std's prebuilt hook objects are made in that home, as a first build makes
# them) and one thread, and the size of each built binary. Linux only (valgrind).
#
#   bench/inspect/build_cost.sh WORK NAME=TREE=VELT ... -- PROGRAM.velt ...
#
# Each side is NAME=TREE=VELT as in rows.sh. Prints `row<TAB>side<TAB>value`, one line per row
# and side; the sides take turns.
set -euo pipefail
[ $# -ge 3 ] || { echo "usage: bench/inspect/build_cost.sh WORK NAME=TREE=VELT ... -- PROGRAM ..." >&2; exit 2; }
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
  for mode in check debug release; do
    for side in "${SIDES[@]}"; do
      IFS='=' read -r label tree velt <<< "$side"
      home=$(mktemp -d "$WORK/home.XXXXXX")
      exe="$WORK/$label-$name-$mode"
      case $mode in
        check) args=(check "$p") ;;
        debug) args=(build "$p" -o "$exe") ;;
        release) args=(build --release "$p" -o "$exe") ;;
      esac
      n=$(
        export VELT_HOME="$home" VELT_STD="$tree/std" VELT_BUILD_STAMP=off VELT_THREADS=1
        count "$velt" "${args[@]}"
      )
      rm -rf "$home"
      printf '%s %s\t%s\t%s\n' "$mode" "$name" "$label" "$n"
      if [ "$mode" != check ]; then
        printf '%s %s size\t%s\t%s\n' "$mode" "$name" "$label" "$(stat -c %s "$exe")"
        rm -f "$exe"
      fi
    done
  done
done
