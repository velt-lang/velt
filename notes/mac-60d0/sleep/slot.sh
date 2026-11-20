#!/bin/bash
# sleep-home in its own target: build, the new test file, the std_scheduler suites and
# c_export_async, then precheck. Logs beside this script.
L=$(cd "$(dirname "$0")" && pwd)/slot
mkdir -p "$L"
cd /Users/andii/dev/velt/branches/sleep-home || exit 1
export CARGO_BUILD_JOBS=3 SDKROOT=$(xcrun --show-sdk-path) VELT_ZIG=$HOME/.velt/cache/zig/0.17.1/zig-aarch64-macos-0.17.1/zig
cargo build --workspace > "$L/build.log" 2>&1; echo "build exit $?"
cargo test -p veltc --test std_scheduler_sleep_handed > "$L/handed.log" 2>&1; echo "sleep_handed exit $? $(grep 'test result' "$L/handed.log" | cut -c1-70)"
cargo nextest run -p veltc -E 'binary(~std_scheduler) | binary(c_export_async)' --no-fail-fast > "$L/suites.log" 2>&1; echo "suites exit $?"
grep -E "^\s+(FAIL|Summary)" "$L/suites.log" | sort | uniq -c | head -12
cargo xtask precheck > "$L/precheck.log" 2>&1; echo "precheck exit $?"; tail -2 "$L/precheck.log"
df -g / | tail -1 | awk '{print $4" GiB free"}'
