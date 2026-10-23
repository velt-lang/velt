#!/usr/bin/env bash
# Instructions per discriminant read (cachegrind): a literal-built union member and a union
# view, each the difference between 2,000,000 and 1,000,000 reads divided by 1,000,000.
#   bench/union_view/run.sh <velt>
set -euo pipefail
velt=$1
dir=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap rm -rf  EXIT
"$velt" build --release "$dir/discriminant_reads.vlt" -o "$tmp/reads" >/dev/null
count() {
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$tmp/reads" "$1" "$2" 2>&1 |
    sed -n 's/.*I *refs: *//p' | tr -d ','
}
for mode in literal view; do
  a=$(count "$mode" 1000000)
  b=$(count "$mode" 2000000)
  echo "$mode: $(( (b - a) / 1000000 )) instructions per read (I refs $a at 1M, $b at 2M)"
done
