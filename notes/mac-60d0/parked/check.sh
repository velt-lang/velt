#!/bin/bash
# #3274 (parked-task's std) through sleep-home's built binaries (main's compiler; no cargo build):
# the scheduler suites (#3273's own test file left out: it needs #3273's std), c_export_async,
# the goldens, std's own tests from parked-task's tree, the golden of #3274 at every mode.
S=$(cd "$(dirname "$0")" && pwd)
L=$S/check; mkdir -p "$L"
P=/Users/andii/dev/velt/branches/parked-task
export SDKROOT=$(xcrun --show-sdk-path) CARGO_BUILD_JOBS=3 VELT_STD=$P/std
cd /Users/andii/dev/velt/branches/sleep-home || exit 1
cargo nextest run -p veltc -E '(binary(~std_scheduler) - binary(std_scheduler_sleep_handed)) | binary(c_export_async) | binary(golden)' --no-fail-fast > "$L/suites.log" 2>&1
echo "suites exit $?"; grep -E "Summary|^\s+FAIL \[" "$L/suites.log" | sort -u | head -10
cd "$P" || exit 1
/Users/andii/dev/velt/branches/sleep-home/target/debug/velt test std > "$L/stdtests.log" 2>&1
echo "std tests exit $? $(tail -1 "$L/stdtests.log")"
bash "$S/run.sh" fix "$P/std" 5
