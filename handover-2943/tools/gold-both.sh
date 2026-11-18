#!/bin/bash
# gold-both.sh <label> <prev>: the goldens with lazy std (the default; for a loader change,
# which every load takes) into val-<label>/gold_lazy.txt, then built from std's unit
# (gold7.sh) and the failing files against val-<prev> (failfiles.sh).
S=/Users/andii/dev/velt/branches/handovers/mac-e688-2943
W=/Users/andii/dev/velt/branches/d1-open-pr
mkdir -p "$S/val-$1"
cd "$W" || exit 1
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
unset VELT_BUILD_STAMP VELT_STD VELT_UNITS VELT_UNITS_SPLIT VELT_UNITS_BUILD VELT_UNITS_DISK
cargo test --workspace --test golden > "$S/val-$1/gold_lazy.txt" 2>&1
echo "lazy gold $?"
grep -E '^golden: [0-9]+ files' "$S/val-$1/gold_lazy.txt"
grep -oE '^tests/golden/\S+\.velt( \[|:)' "$S/val-$1/gold_lazy.txt" | sed -E 's/( \[|:)$//' | sort -u
bash "$S/tools/gold7.sh" "$1" | grep -E "^(build|gold) |^golden: [0-9]+ files"
bash "$S/tools/failfiles.sh" "val-$2" "val-$1"
