#!/usr/bin/env bash
# Every std_scheduler* suite of veltc in worktree $1, light (JOBS=3, no incremental); one line per
# suite result plus failures.
set -uo pipefail
cd "$1" || exit 1
args=()
for f in crates/veltc/tests/std_scheduler*.rs; do
  n=$(basename "$f" .rs)
  args+=(--test "$n")
done
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 cargo test -p veltc --no-fail-fast "${args[@]}" 2>&1 |
  grep -E '^test result|FAILED|panicked|^error' | sort | uniq -c
