#!/bin/bash
# gold7.sh <label>: build velt, then the debug goldens with builds from std's unit on disk
# (units-passes, worktree d1-open-pr); output in val-<label>/gold_disk.txt + fail_files.txt.
S=/Users/andii/dev/velt/branches/handovers/mac-e688-2943
W=/Users/andii/dev/velt/branches/d1-open-pr
U=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ugold
L=$S/val-$1
mkdir -p "$L"
cd "$W" || exit 1
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
unset VELT_BUILD_STAMP VELT_STD
cargo build -p veltc --bin velt > "$L/build.log" 2>&1
echo "build $?"
rm -rf "${U:?}"
VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=$U \
  cargo test --workspace --test golden > "$L/gold_disk.txt" 2>&1
echo "gold $?"
grep -E '^golden: ' "$L/gold_disk.txt"
grep -oE '^tests/golden\S+ \[\w+\]' "$L/gold_disk.txt" | sed 's/ \[.*//' | sort -u > "$L/fail_files.txt"
wc -l < "$L/fail_files.txt"
