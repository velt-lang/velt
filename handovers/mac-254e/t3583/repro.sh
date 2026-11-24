#!/bin/bash
# #3583 train failure: sched_node_done.test.velt --release with a fresh VELT_HOME, as std_tests runs it.
set -u
W=$HOME/dev/velt/branches/fix-3420-254e
S=$SCRATCH/t3583
export CARGO_TARGET_DIR=$W/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$W" || exit 1
echo "== build $(date +%T) at $(git log --oneline -1 | cut -c1-60)"
cargo build -q -p veltc --bin velt || exit 1
cargo build -q -p velt_rt || exit 1
cargo build -q --release -p velt_rt || exit 1
for mode in release debug; do
  rm -rf "$S/home" && mkdir -p "$S/home"
  flag=""; [ "$mode" = release ] && flag=--release
  echo "== $mode, fresh home $(date +%T)"
  perl -e 'alarm shift; exec @ARGV' 600 env VELT_HOME="$S/home" VELT_REGISTRY="$S/home/registry" NO_COLOR=1 VELT_STD="$W/std" \
    target/debug/velt test $flag std/lang/hooks/sched_node_done.test.velt > "$S/$mode.log" 2>&1
  echo "exit $?"
  grep -E "ICE|test result|panicked|FAIL" "$S/$mode.log" | head -5
done
echo "=== done $(date +%T)"
