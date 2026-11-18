#!/bin/bash
# la-diff.sh <golden .velt>: the std functions local_async's graph takes under lazy std and
# under units (VELT_TMP_LA_DUMP, the temporary dump), and the difference of the two.
W=/Users/andii/dev/velt/branches/d1-open-pr
D=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/la
mkdir -p "$D"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk VELT_STD=$W/std VELT_TMP_LA_DUMP=1 VELT_BUILD_CACHE=off
"$W/target/debug/velt" build "$1" -o "$D/o-lazy" 2>&1 | grep '^LA-STD' | sort -u > "$D/lazy.txt"
VELT_TMP_LA_DUMP= VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK="$D/u" "$W/target/debug/velt" build "$1" -o "$D/o-warm" > /dev/null 2>&1
VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK="$D/u" \
  "$W/target/debug/velt" build "$1" -o "$D/o-units" 2>&1 | grep '^LA-STD' | sort -u > "$D/units.txt"
wc -l "$D/lazy.txt" "$D/units.txt"
echo "--- only under units:"
comm -13 "$D/lazy.txt" "$D/units.txt" | head -40
echo "--- only under lazy:"
comm -23 "$D/lazy.txt" "$D/units.txt" | head -40
