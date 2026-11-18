#!/bin/bash
# one-units.sh <golden .velt> [VAR=value ...]: `velt build` of one golden from std's unit on disk
# (the env gold7.sh uses, a fresh unit dir), then runs it; extra VAR=value pairs are exported
# first (e.g. VELT_UNITS_WALK_STD=0). Prints the build's diagnostics and the program's output.
W=/Users/andii/dev/velt/branches/d1-open-pr
U=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/u-one
O=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/one-out
f=$1
shift
for kv in "$@"; do export "${kv?}"; done
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk VELT_STD=$W/std
export VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=$U
mkdir -p "$U"
rm -f "$O"
"$W/target/debug/velt" build "$f" -o "$O" 2>&1 | head -30
echo "build exit ${PIPESTATUS[0]}"
[ -x "$O" ] && "$O" 2>&1 | head -20
