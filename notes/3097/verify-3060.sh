#!/usr/bin/env bash
# #3060's fix: the rescue suite with the fix, then the new test with sched_wake.velt reverted (it
# must fail), then the fix restored.
set -uo pipefail
WT=/Users/andii/dev/velt/branches/lazy-output-order
F=std/lang/hooks/sched_wake.velt
KEEP=/private/tmp/claude-501/-Users-andii-dev-velt-main/505bcb47-021f-409e-bf01-c29e5c3a75e5/scratchpad/sched_wake.fixed.velt
cd "$WT" || exit 1
echo "== with the fix: the rescue suite"
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 cargo test -p veltc --test std_scheduler_rescue 2>&1 | grep -E '^test |test result' | grep -v ' ok$'
cp "$F" "$KEEP"
git checkout -- "$F"
echo "== without the fix: the new test (must fail)"
CARGO_INCREMENTAL=0 cargo test -p veltc --test std_scheduler_rescue a_task_woken_by_a_spare 2>&1 | grep -E 'test result|not lazy' | cut -c1-160
cp "$KEEP" "$F"
git diff --stat -- "$F"
