#!/bin/bash
# errdiff.sh <file>...: full diagnostics of `velt check` under lazy std and under units from
# disk (the golden harness's mode), side by side as a unified diff (velt-units lines dropped).
R=/Users/andii/dev/velt/branches/d1-open-pr
U=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ugold
O=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/errdiff
mkdir -p "$O"
export VELT_STD=$R/std
for f in "$@"; do
  b=$(basename "$f" .velt)
  "$R/target/debug/velt" check "$f" 2>&1 | grep -v '^velt' > "$O/$b.lazy"
  VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=$U "$R/target/debug/velt" check "$f" 2>&1 | grep -v '^velt' > "$O/$b.units"
  echo "=== $f"
  diff -u "$O/$b.lazy" "$O/$b.units" | tail -n +3 | head -30
done
