#!/bin/bash
# firsterr.sh <file>...: velt run under units (disk build) and print each file's first error,
# ICE or panic line, and the first differing output line against the golden .out.
R=/Users/andii/dev/velt/branches/d1-open-pr
U=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/u6
O=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/firsterr.out
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
for f in "$@"; do
  E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=$U)
  env "${E[@]}" "$R/target/debug/velt" run "$f" > "$O" 2> "$O.err"
  echo "== $f exit=$?"
  grep -v '^velt-units' "$O.err" | grep -m2 -E 'error|ICE|panicked|panic:' | cut -c1-200
  diff "$O" "${f%.velt}.out" | head -4
done
