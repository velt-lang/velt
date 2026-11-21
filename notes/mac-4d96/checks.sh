#!/bin/bash
# #3273 with the cuts (after mac-60d0's merge-check.sh): build, the PR's test file, the scheduler
# suites and c_export_async, the sleep/timer goldens, precheck. Logs in checks/; a summary on stdout.
S=$(cd "$(dirname "$0")" && pwd)
L=$S/checks; mkdir -p "$L"
cd /Users/andii/dev/velt/branches/sleep-home || exit 1
export SDKROOT=$(xcrun --show-sdk-path) CARGO_BUILD_JOBS=3
git log --oneline -1
cargo build --workspace > "$L/build.log" 2>&1; echo "build exit $?"
cargo test -p veltc --test std_scheduler_sleep_handed > "$L/handed.log" 2>&1; echo "handed exit $? $(grep 'test result' "$L/handed.log" | tail -1)"
cargo nextest run -p veltc -E 'binary(~std_scheduler) | binary(c_export_async)' --no-fail-fast > "$L/suites.log" 2>&1
echo "suites exit $?"; grep -E "Summary|^\s+FAIL \[" "$L/suites.log" | sort -u | head -8
for g in sleep timer; do VELT_GOLDEN=$g cargo test -q -p veltc --test golden > "$L/golden-$g.log" 2>&1; echo "golden $g exit $? $(grep 'test result' "$L/golden-$g.log" | tail -1)"; done
cargo xtask precheck > "$L/precheck.log" 2>&1; echo "precheck exit $?"; tail -2 "$L/precheck.log"
df -h /Users | tail -1 | awk '{print $4 " free"}'
