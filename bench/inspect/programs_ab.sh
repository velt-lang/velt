#!/usr/bin/env bash
# Run cost of programs built by one compiler against another: cachegrind instructions of each
# program built with `velt build --release` (empty VELT_HOME, build cache off), run with one
# thread, and whether the sides print the same. Linux only (valgrind).
#
#   bench/inspect/programs_ab.sh WORK NAME=TREE=VELT NAME=TREE=VELT -- PROGRAM.velt ...
#
# Each side is NAME=TREE=VELT as in rows.sh. Prints `program<TAB>side<TAB>instructions` per side,
# then `program<TAB>same|DIFF`.
set -euo pipefail
[ $# -ge 4 ] || { echo "usage: bench/inspect/programs_ab.sh WORK A=TREE=VELT B=TREE=VELT -- PROGRAM ..." >&2; exit 2; }
WORK=$1; shift
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
SIDES=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do SIDES+=("$1"); shift; done
shift
for p in "$@"; do
  name=$(basename "$p" .velt)
  outs=()
  for side in "${SIDES[@]}"; do
    IFS='=' read -r label tree velt <<< "$side"
    exe="$WORK/$label-$name"
    home=$(mktemp -d "$WORK/home.XXXXXX")
    (
      export VELT_HOME="$home" VELT_STD="$tree/std" VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off
      "$velt" build --release "$p" -o "$exe" > /dev/null
    )
    rm -rf "$home"
    n=$(VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
      "$exe" 2>&1 > "$exe.out" | sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,)
    printf '%s\t%s\t%s\n' "$name" "$label" "$n"
    outs+=("$exe.out")
  done
  if cmp -s "${outs[0]}" "${outs[1]}"; then printf '%s\tsame\n' "$name"; else printf '%s\tDIFF\n' "$name"; fi
  rm -f "$WORK/"*"-$name" "$WORK/"*"-$name.out"
done
