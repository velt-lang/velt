#!/bin/bash
# val.sh <label> <prev>: the full validation of branches/d1-open-pr (units-passes):
# goldens with lazy std and from std's unit (the handover's tools/gold-both.sh, logs in
# branches/handovers/mac-e688-2943/val-<label>), then the tests of velt_sema and velt_loader,
# veltc's units tests, and clippy. Every step's exit status is printed.
H=/Users/andii/dev/velt/branches/handovers/mac-e688-2943
W=/Users/andii/dev/velt/branches/d1-open-pr
L=$H/val-$1
mkdir -p "$L" /private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad
bash "$H/tools/gold-both.sh" "$1" "$2"
cd "$W" || exit 1
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
unset VELT_BUILD_STAMP VELT_STD VELT_UNITS VELT_UNITS_SPLIT VELT_UNITS_BUILD VELT_UNITS_DISK
cargo test -p velt_sema -p velt_loader > "$L/crate_tests.txt" 2>&1
echo "sema+loader tests $?"
grep -E "^test result|FAILED|panicked" "$L/crate_tests.txt" | sort | uniq -c | tail -12
cargo test -p veltc --test units --test units_globals > "$L/units_tests.txt" 2>&1
echo "units tests $?"
grep -E "^test |^test result" "$L/units_tests.txt" | tail -20
cargo clippy -p velt_sema -p velt_loader -p veltc --all-targets -- -D warnings > "$L/clippy.txt" 2>&1
echo "clippy $?"
grep -E "^(error|warning)" -A6 "$L/clippy.txt" | head -40
