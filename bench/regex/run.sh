#!/usr/bin/env bash
# Instruction counts (cachegrind, Linux) of regex programs built by the two trees that
# bench/compare_refs.sh exported into WORK_DIR ({base,head}, compilers in target-{base,head}):
# regex-redux on the output of `fasta 100000`, and bench/regex/literal.vlt (a loop of
# literal-pattern replace/test/split calls through the RegExp API). Run compare_refs.sh first:
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
cp "$HERE/literal.vlt" "$OUT/literal.vlt"
for side in base head; do
  velt="$WORK/target-$side/release/velt"
  (cd "$WORK/$side" && "$velt" build --release bench/benchmarks-game/fasta/main.vlt -o "$OUT/fasta-$side" >/dev/null)
  (cd "$WORK/$side" && "$velt" build --release bench/benchmarks-game/regex-redux/main.vlt -o "$OUT/redux-$side" >/dev/null)
  (cd "$WORK/$side" && "$velt" build --release "$OUT/literal.vlt" -o "$OUT/literal-$side" >/dev/null)
done
"$OUT/fasta-base" 100000 > "$OUT/fasta.txt"
count() {
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
    awk '/I *refs:/ { gsub(",", "", $NF); print $NF }'
}
for prog in redux literal; do
  b=$(VELT_THREADS=1 count "$OUT/$prog-base" < "$OUT/fasta.txt")
  h=$(VELT_THREADS=1 count "$OUT/$prog-head" < "$OUT/fasta.txt")
  awk -v p="$prog" -v b="$b" -v h="$h" 'BEGIN { printf "%s base %d head %d change %+.2f%%\n", p, b, h, (h - b) * 100 / b }'
done
