#!/bin/bash
# #3487 arm64 rows on this Mac: a release velt and runtime from the worktree, each row built with
# main's std (perf/base) and the change's (perf/new); instructions retired (/usr/bin/time -l) at
# VELT_THREADS=1, median of 5 interleaved, and at 4 workers, median of 5.
set -u
W=$HOME/dev/velt/branches/fix-3420-254e
P=$HOME/dev/velt/branches/r3420-254e/perf
ROWS=${ROWS:-"race_select race_small race_yield await_chain await_deep fanout_all spawn_many pingpong_pinned promise_identity_loop all_small_stored"}
export CARGO_TARGET_DIR=$W/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$W" || exit 1
if [ "${SKIP_BUILD:-}" = "" ]; then
  cargo build --release -q -p velt_rt || exit 1
  cargo build --release -q -p veltc || exit 1
fi
V=$W/target/release/velt
mkdir -p "$P/bin"
instr() { # <exe> <threads>
  VELT_THREADS=$2 /usr/bin/time -l "$1" 2>&1 > /dev/null | awk '/instructions retired/ {print $1}'
}
med() { sort -n | awk '{a[NR]=$1} END {print a[int((NR+1)/2)]}'; }
for r in $ROWS; do
  for s in base new; do
    VELT_BUILD_CACHE=off VELT_STD=$P/$s/std VELT_RT_LIB=$W/target/release/libvelt_rt.a "$V" build --release "$W/bench/async/$r.velt" -o "$P/bin/$r-$s" > "$P/bin/$r-$s.log" 2>&1 || { echo "$r $s build failed"; tail -3 "$P/bin/$r-$s.log"; continue 2; }
    echo "  $r $s: binary $(stat -f %z "$P/bin/$r-$s") bytes"
  done
  for th in 1 4; do
    : > "$P/bin/b.txt"; : > "$P/bin/n.txt"
    for i in 1 2 3 4 5; do
      instr "$P/bin/$r-base" $th >> "$P/bin/b.txt"
      instr "$P/bin/$r-new" $th >> "$P/bin/n.txt"
    done
    b=$(med < "$P/bin/b.txt"); n=$(med < "$P/bin/n.txt")
    echo "$r T$th: base $b new $n  $(( (n - b) * 10000 / b ))bp"
  done
done
