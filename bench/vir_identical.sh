#!/usr/bin/env bash
# Do two compilers emit the same VIR for the programs under bench/? Prints each program whose
# VIR differs and a summary (programs the base compiler cannot build are counted apart).
#
#   bench/vir_identical.sh <base velt> <head velt> <base std dir> <head std dir> [<dir> <program>...]
#
# With <dir> and programs (paths relative to it), those are compared instead, for example the
# goldens that never write `undefined`:
#   (cd tests/golden && grep -L undefined m*/*.vlt lang/*.vlt std/*.vlt) |
#     xargs bench/vir_identical.sh <base velt> <head velt> <base std> <head std> tests/golden
#
# The VIR holds the paths of std files (in panic locations, " at <std>/prelude/array.vlt:127:22",
# and in their lengths), so each compiler reads its std from a copy at one and the same path
# ($VELT_STD): the base's for every base build, then the head's for every head build.
# $JOBS compilers run at a time (default 8).
set -uo pipefail
base=$1
head=$2
base_std=$(cd "$3" && pwd)
head_std=$(cd "$4" && pwd)
if [ $# -gt 4 ]; then
  cd "$5"
  shift 5
  programs=("$@")
else
  cd "$(dirname "$0")"
  programs=(*.vlt */*.vlt)
fi
tmp=$(mktemp -d)
mkdir -p "$tmp/base" "$tmp/head"

# Emit the VIR of every program with compiler $1 and std $2 into directory $3.
emit_all() {
  rm -rf "$tmp/std"
  cp -r "$2" "$tmp/std"
  local n=0
  for f in "${programs[@]}"; do
    (VELT_STD="$tmp/std" "$1" build "$f" --emit vir >"$3/${f//\//_}.vir" 2>/dev/null ||
      rm -f "$3/${f//\//_}.vir") &
    n=$((n + 1))
    [ $((n % ${JOBS:-8})) -eq 0 ] && wait
  done
  wait
}

# The glue that disposes of an unclaimed promise result is named after its type's index in the
# compiler's type table (`_Gunclaimed_205`), which moves whenever the standard library declares
# one more type: the same function under another name, so the index is left out of the comparison.
names() {
  sed -E 's/_Gunclaimed_[0-9]+/_Gunclaimed_N/g' "$1"
}

emit_all "$base" "$base_std" "$tmp/base"
emit_all "$head" "$head_std" "$tmp/head"
same=0 differ=0 skipped=0
for f in "${programs[@]}"; do
  v=${f//\//_}.vir
  if [ ! -f "$tmp/base/$v" ]; then
    skipped=$((skipped + 1))
  elif cmp -s <(names "$tmp/base/$v") <(names "$tmp/head/$v"); then
    same=$((same + 1))
  else
    differ=$((differ + 1))
    echo "differs: $f"
  fi
done
rm -rf "$tmp"
echo "identical=$same differ=$differ not-built=$skipped"
