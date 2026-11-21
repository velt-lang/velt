#!/bin/bash
# #3360's fix (settle-race cf5424436) checked in the sleep-home worktree, detached (same
# compiler, built target): build, its test file, the scheduler suites, the promise goldens,
# std's own tests, precheck. The worktree goes back to sleep-home at the end.
S=$(cd "$(dirname "$0")" && pwd)
L=$S/checks3360; mkdir -p "$L"
cd /Users/andii/dev/velt/branches/sleep-home || exit 1
[ -z "$(git status --porcelain)" ] || { echo "sleep-home is not clean"; exit 1; }
git checkout -q --detach settle-race || exit 1
export SDKROOT=$(xcrun --show-sdk-path) CARGO_BUILD_JOBS=3
git log --oneline -1
cargo build --workspace > "$L/build.log" 2>&1; echo "build exit $?"
cargo test -p veltc --test std_scheduler_handed_promises > "$L/handed.log" 2>&1; echo "handed_promises exit $? $(grep 'test result' "$L/handed.log" | tail -1)"
cargo nextest run -p veltc -E 'binary(~std_scheduler) | binary(c_export_async)' --no-fail-fast > "$L/suites.log" 2>&1
echo "suites exit $?"; grep -E "Summary|^\s+FAIL \[" "$L/suites.log" | sort -u | head -8
VELT_GOLDEN=promise cargo test -q -p veltc --test golden > "$L/golden-promise.log" 2>&1; echo "golden promise exit $? $(grep 'test result' "$L/golden-promise.log" | tail -1)"
VELT_STD=$PWD/std ./target/debug/velt test std > "$L/stdtests.log" 2>&1; echo "velt test std exit $? $(tail -1 "$L/stdtests.log")"
cargo xtask precheck > "$L/precheck.log" 2>&1; echo "precheck exit $?"; tail -2 "$L/precheck.log"
git checkout -q sleep-home; git status -sb | head -1
df -g / | tail -1 | awk '{print $4 " GiB free"}'
