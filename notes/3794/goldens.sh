#!/bin/bash
# Golden subsets touching promises and tasks, then the hooks' white-box std tests.
cd /Users/andii/dev/velt/branches/let-go-knobs || exit 1
export CARGO_TARGET_DIR=/Users/andii/dev/velt/branches/s13-target CARGO_BUILD_JOBS=3 RUST_TEST_THREADS=3
for g in promise async spawn task; do
  echo "== VELT_GOLDEN=$g"
  VELT_GOLDEN=$g perl -e 'alarm shift; exec @ARGV' 3000 cargo test -p veltc --test golden 2>&1 | grep -E "test result|passed|failed|FAIL" | tail -5
done
echo "== velt test std/lang/hooks"
VELT_STD=$PWD/std VELT_RT_LIB=$CARGO_TARGET_DIR/debug/libvelt_rt.a perl -e 'alarm shift; exec @ARGV' 1800 $CARGO_TARGET_DIR/debug/velt test std/lang/hooks 2>&1 | tail -5
