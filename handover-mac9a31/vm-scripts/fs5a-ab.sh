#!/bin/bash
# Inside the OrbStack VM: release velt + runtime for base (branches/ab-fs5a-base, the merge base)
# and head (branches/fs-sync-2196), each in its own $HOME target; then cachegrind's instruction
# count of bench/stdport/fs_whole.velt (head's copy, which base runs on the runtime's functions).
set -u
source "$HOME/.cargo/env"
B=$VELT_ROOT/branches
OUT=$HOME/fs5a-out
mkdir -p "$OUT"
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 VELT_BUILD_CACHE=off VELT_THREADS=1
cp "$B/fs-sync-2196/bench/stdport/fs_whole.velt" "$OUT/fs_whole.velt"
for side in base head; do
  if [ $side = base ]; then tree=$B/ab-fs5a-base; else tree=$B/fs-sync-2196; fi
  echo "== build $side"
  (cd "$tree" && CARGO_TARGET_DIR=$HOME/tgt-fs5a-$side cargo build --release -p veltc --bin velt 2>&1 | tail -1) || exit 1
  (cd "$tree" && CARGO_TARGET_DIR=$HOME/tgt-fs5a-$side cargo build --release -p velt_rt 2>&1 | tail -1) || exit 1
  VELT_STD=$tree/std VELT_RT_LIB=$HOME/tgt-fs5a-$side/release/libvelt_rt.a \
    "$HOME/tgt-fs5a-$side/release/velt" build "$OUT/fs_whole.velt" --release -o "$OUT/fs_whole-$side" || exit 1
  echo "$side prints: $("$OUT/fs_whole-$side")"
done
for run in 1 2; do
  for side in base head; do
    valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
      "$OUT/fs_whole-$side" > "$OUT/$side-$run.log" 2>&1
    echo "fs_whole-$side-$run $(grep -o 'I *refs: *[0-9,]*' "$OUT/$side-$run.log" | tr -dc '0-9')"
  done
done
