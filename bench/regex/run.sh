#!/usr/bin/env bash
# Instruction counts (cachegrind, Linux) of regex programs built by the two trees that
# bench/compare_refs.sh exported into WORK_DIR ({base,head}, compilers in target-{base,head}):
# regex-redux on the output of `fasta 100000`, bench/regex/literal.velt (a loop of
# literal-pattern replace/test/split calls through the RegExp API), bench/regex/units.velt, and
# each row of bench/regex/long.velt (patterns that are no literal over a subject of 100,000
# characters, failing and succeeding) and of bench/regex/lines.velt (such patterns searched line
# by line), and bench/regex/deep_copy.velt (deep copies of an object holding regexes). Run
# compare_refs.sh first:
#
#   bench/compare_refs.sh BASE_REF HEAD_REF WORK_DIR
#   bench/regex/run.sh WORK_DIR
#
# Prints one line per program: "<name> base <instructions> head <instructions> change <+x.xx%>".
set -euo pipefail
if [ $# -ne 1 ]; then
  echo "usage: bench/regex/run.sh WORK_DIR" >&2
  exit 2
fi
WORK=$(cd "$1" && pwd)
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=$WORK/regex
mkdir -p "$OUT"
cp "$HERE/literal.velt" "$HERE/units.velt" "$HERE/long.velt" "$HERE/lines.velt" "$HERE/deep_copy.velt" "$OUT/"
for side in base head; do
  velt="$WORK/target-$side/release/velt"
  (cd "$WORK/$side" && "$velt" build --release bench/benchmarks-game/fasta/main.velt -o "$OUT/fasta-$side" >/dev/null)
  (cd "$WORK/$side" && "$velt" build --release bench/benchmarks-game/regex-redux/main.velt -o "$OUT/redux-$side" >/dev/null)
  for prog in literal units long lines deep_copy; do
    (cd "$WORK/$side" && "$velt" build --release "$OUT/$prog.velt" -o "$OUT/$prog-$side" >/dev/null)
  done
done
"$OUT/fasta-base" 100000 > "$OUT/fasta.txt"
count() {
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    awk '/I *refs:/ { gsub(",", "", $NF); print $NF }'
}
row() {
  awk -v p="$1" -v b="$2" -v h="$3" 'BEGIN { printf "%s base %d head %d change %+.2f%%\n", p, b, h, (h - b) * 100 / b }'
}
for prog in redux literal units deep_copy; do
  b=$(VELT_THREADS=1 count "$OUT/$prog-base" < "$OUT/fasta.txt")
  h=$(VELT_THREADS=1 count "$OUT/$prog-head" < "$OUT/fasta.txt")
  row "$prog" "$b" "$h"
done
for prog in long lines; do
  rows="0 1 2 3 4 5 6 7"
  if [ "$prog" = lines ]; then
    rows="$rows 8 9"
  fi
  for r in $rows; do
    b=$(VELT_THREADS=1 count "$OUT/$prog-base" "$r")
    h=$(VELT_THREADS=1 count "$OUT/$prog-head" "$r")
    row "$prog-$r" "$b" "$h"
  done
done
