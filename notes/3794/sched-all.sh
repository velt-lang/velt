#!/bin/bash
# Every std_scheduler_* test file of veltc, three test threads, my target dir.
cd /Users/andii/dev/velt/branches/let-go-knobs || exit 1
args=()
for f in crates/veltc/tests/std_scheduler*.rs; do b=$(basename "$f" .rs); args+=(--test "$b"); done
CARGO_TARGET_DIR=/Users/andii/dev/velt/branches/s13-target CARGO_BUILD_JOBS=3 perl -e 'alarm shift; exec @ARGV' 3500 cargo test -p veltc --no-fail-fast "${args[@]}" -- --test-threads=3
