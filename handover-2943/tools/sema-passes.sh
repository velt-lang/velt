#!/bin/bash
# sema-passes.sh <velt> <program.velt>: sema's pass timings (VELT_SEMA_TIMINGS) of a warm
# `velt check` of a fresh copy of the program, lazy std and from std's unit on disk (built by a
# first check, then a second measured; the "loaded from disk" line confirms the unit was read),
# each median of 5 runs per pass, side by side, largest difference first.
V=$1
P=$2
R=/Users/andii/dev/velt/branches/d1-open-pr
D=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/sema-passes
rm -rf "$D" && mkdir -p "$D/u"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk VELT_STD=$R/std VELT_SEMA_THREADS=1
U=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK="$D/u")
run() { # mode n
  cp "$P" "$D/p.velt"
  echo "// $1 $2 $(date +%s%N)" >> "$D/p.velt"
  case $1 in
    lazy) VELT_SEMA_TIMINGS=1 "$V" check "$D/p.velt" ;;
    units) env "${U[@]}" VELT_SEMA_TIMINGS=1 "$V" check "$D/p.velt" ;;
  esac > "$D/$1-$2.txt" 2>&1
}
env "${U[@]}" "$V" check "$P" > /dev/null 2>&1
for i in 1 2 3 4 5; do run lazy $i; run units $i; done
grep -c "loaded from disk" "$D"/units-*.txt | tr '\n' ' '
echo
python3 -I "$(dirname "$0")/sema_passes.py" "$D"
