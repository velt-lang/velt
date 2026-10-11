#!/usr/bin/env bash
# Do two compilers generate the same VIR? For a change that should leave existing programs'
# code alone (a type-checking feature): prints `same` or `DIFF` per program, then the counts.
#
#   bench/vir_diff.sh BASE_VELT HEAD_VELT [program.vlt...]
#
# Without programs it compares every `.vlt` under bench/ (run from the repository root). Each
# program is built with `velt build --release --emit vir` by both compilers (each with the std of
# the tree it was built from, or VELT_STD); a program neither compiler builds is `FAIL`.
set -uo pipefail
base=${1:?usage: bench/vir_diff.sh BASE_VELT HEAD_VELT [program.vlt...]}
head=${2:?usage: bench/vir_diff.sh BASE_VELT HEAD_VELT [program.vlt...]}
shift 2
if [ $# -eq 0 ]; then
  mapfile -t progs < <(find bench -name '*.vlt' | sort)
else
  progs=("$@")
fi
work=$(mktemp -d)
same=0
diff=0
fail=0
for p in "${progs[@]}"; do
  "$base" build --release --emit vir "$p" > "$work/base.vir" 2> /dev/null
  b=$?
  "$head" build --release --emit vir "$p" > "$work/head.vir" 2> /dev/null
  h=$?
  if [ $b -ne 0 ] && [ $h -ne 0 ]; then
    echo "FAIL $p"
    fail=$((fail + 1))
  elif [ $b -eq $h ] && cmp -s "$work/base.vir" "$work/head.vir"; then
    echo "same $p"
    same=$((same + 1))
  else
    echo "DIFF $p"
    diff=$((diff + 1))
  fi
done
rm -rf "$work"
echo "same $same, different $diff, failed to build $fail"
[ $diff -eq 0 ]
