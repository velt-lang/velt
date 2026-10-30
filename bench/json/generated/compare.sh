#!/usr/bin/env bash
# Instructions retired by bench/json/stringify.vlt (the compiler's JSON glue) and by
# generated/stringify.vlt (the same work through the source-generated `stringify` of
# ./index.vlt), built by the same compiler with the same runtime, best of RUNS.
#
#   bench/json/generated/compare.sh [runs]
#
# macOS: /usr/bin/time -l; Linux: valgrind --tool=cachegrind. Set VELT_RT_LIB to a release
# runtime (target/release/libvelt_rt.a): the debug runtime's builder calls dominate otherwise.
set -euo pipefail
RUNS=${1:-3}
HERE=$(cd "$(dirname "$0")" && pwd)
VELT=${VELT:-velt}
OUT=$(mktemp -d)
"$VELT" build --release "$HERE/../stringify.vlt" -o "$OUT/glue"
"$VELT" build --release "$HERE/stringify.vlt" -o "$OUT/generated"
"$VELT" build --release "$HERE/check.vlt" -o "$OUT/check"
# The generated writer must write what JSON.stringify writes (check.vlt prints pairs).
"$OUT/check" | awk 'NR % 2 == 1 { a = $0 } NR % 2 == 0 && $0 != a { print "differs: " a " / " $0; bad = 1 } END { exit bad }'
count() {
  if [ "$(uname)" = Darwin ]; then
    /usr/bin/time -l "$1" 2>&1 >/dev/null | awk '/instructions retired/ { print $1 }'
  else
    valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$1" 2>&1 >/dev/null |
      awk '/I *refs:/ { gsub(",", "", $4); print $4 }'
  fi
}
for name in glue generated; do
  best=
  for _ in $(seq "$RUNS"); do
    n=$(count "$OUT/$name")
    if [ -z "$best" ] || [ "$n" -lt "$best" ]; then best=$n; fi
  done
  echo "$name: $best instructions"
done
rm -rf "$OUT"
