#!/usr/bin/env bash
# JSON and number fuzz against Node: bench/json/fuzz/strings.vlt (JSON.stringify of random
# strings: escapes, surrogates, non-ASCII; from #1066), numbers.vlt (of random doubles),
# to_number.vlt (Number, parseFloat and JSON.parse of numeric strings), all valid TypeScript,
# and values.vlt (JSON.parseValue of random documents from values_gen.py, written back with
# their keys; values.js is Node's side), under Node and under Velt, debug (with the checking
# allocator) and --release. Every output must be Node's, byte for byte. Prints one line per run
# and exits non-zero on a difference.
#
#   bench/json/fuzz/run.sh [VELT]     (VELT defaults to `velt` on PATH)
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
VELT=${1:-velt}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
status=0
python3 -I "$HERE/values_gen.py" 1660 20000 > "$WORK/values.txt"
cp "$HERE/values.js" "$WORK/values.js"
for prog in strings numbers to_number values; do
  cp "$HERE/$prog.vlt" "$WORK/$prog.vlt"
  if [[ $prog == values ]]; then
    (cd "$WORK" && node values.js > "$prog.node")
  else
    cp "$HERE/$prog.vlt" "$WORK/$prog.ts"
    (cd "$WORK" && node "$prog.ts" > "$prog.node")
  fi
  echo "$prog: node $(wc -l < "$WORK/$prog.node" | tr -d ' ') lines"
  for mode in debug release; do
    flag=""
    [[ $mode == release ]] && flag="--release"
    # shellcheck disable=SC2086 # an empty flag is no word
    (cd "$WORK" && VELT_RT_DEBUG_ALLOC=1 "$VELT" run $flag "$prog.vlt" > "$prog.$mode")
    if cmp -s "$WORK/$prog.node" "$WORK/$prog.$mode"; then
      echo "  $mode: same"
    else
      echo "  $mode: DIFFERENT"
      status=1
    fi
  done
done
exit $status
