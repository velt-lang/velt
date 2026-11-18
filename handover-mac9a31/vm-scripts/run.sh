#!/bin/bash
# Inside the OrbStack VM: release velt + runtime for base (origin/main, branches/ab-stdin-base) and
# head (branches/stdin-2196), each in its own $HOME target; then cachegrind's instruction count of
# lines.velt reading 1M piped lines (500k by readLineSync, the rest by readAllSync).
set -u
source "$HOME/.cargo/env"
B=$VELT_ROOT/branches
V=$B/vm-stdin
OUT=$HOME/stdin-out
mkdir -p "$OUT"
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 VELT_BUILD_CACHE=off VELT_THREADS=1
[ -f "$OUT/lines.txt" ] || seq 1 1000000 | sed 's/^/line number /' > "$OUT/lines.txt"
for side in base head; do
  if [ $side = base ]; then tree=$B/ab-stdin-base; else tree=$B/stdin-2196; fi
  echo "== build $side"
  (cd "$tree" && CARGO_TARGET_DIR=$HOME/tgt-stdin-$side cargo build --release -p veltc --bin velt 2>&1 | tail -1) || exit 1
  (cd "$tree" && CARGO_TARGET_DIR=$HOME/tgt-stdin-$side cargo build --release -p velt_rt 2>&1 | tail -1) || exit 1
  VELT_STD=$tree/std VELT_RT_LIB=$HOME/tgt-stdin-$side/release/libvelt_rt.a \
    "$HOME/tgt-stdin-$side/release/velt" build "$V/lines.velt" --release -o "$OUT/lines-$side" || exit 1
  echo "$side: $("$OUT/lines-$side" < "$OUT/lines.txt")"
done
for run in 1 2; do
  for side in base head; do
    valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
      "$OUT/lines-$side" < "$OUT/lines.txt" > "$OUT/$side-$run.log" 2>&1
    echo "lines-$side-$run $(grep -o 'I *refs: *[0-9,]*' "$OUT/$side-$run.log" | tr -dc '0-9')"
  done
done
