#!/usr/bin/env bash
# The slow paths of JavaScript's key order (#756) that the nightly suite does not cover: setting
# many array-index keys on a JsonValue, and reading records without such keys. Builds each
# program with every velt given (LLVM release), checks that it prints what Node prints, and
# prints the times each run reports (stderr), RUNS runs each, with Node's.
#
#   bench/key_order/run.sh VELT [VELT...]      (RUNS=3 by default)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
RUNS=${RUNS:-3}
OUT=${TMPDIR:-/tmp}/velt-key-order
mkdir -p "$OUT"
for prog in set_index_keys record_reads; do
  node "$HERE/$prog.js" > "$OUT/$prog.expected" 2>/dev/null
  for velt in "$@" node; do
    if [ "$velt" = node ]; then
      cmd=(node "$HERE/$prog.js")
    else
      exe="$OUT/$prog-$(echo "$velt" | md5sum | cut -c1-8)"
      "$velt" build --release --backend llvm "$HERE/$prog.vlt" -o "$exe" > /dev/null
      cmd=("$exe")
    fi
    for _ in $(seq 1 "$RUNS"); do
      "${cmd[@]}" > "$OUT/out" 2> "$OUT/err"
      cmp -s "$OUT/out" "$OUT/$prog.expected" || { echo "$velt: $prog output differs" >&2; exit 1; }
      echo "$velt: $(cat "$OUT/err")"
    done
  done
done
