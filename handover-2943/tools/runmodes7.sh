#!/bin/bash
# runmodes.sh <dir> <file>...: velt run per file, lazy vs units disk build (fresh unit dir once,
# then warm); compares each with the golden .out and prints OK/DIFF and the first error.
D=$1; shift
R=/Users/andii/dev/velt/branches/d1-open-pr
U=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/u5
O=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/runmodes.out
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$D" || exit 1
rm -rf "${U:?}"
for f in "$@"; do
  b=${f%.velt}
  for mode in lazy disk; do
    case $mode in
      lazy) E=() ;;
      disk) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=$U) ;;
    esac
    env "${E[@]}" "$R/target/debug/velt" run "$f" > "$O" 2> "$O.err"
    code=$?
    if cmp -s "$O" "$b.out"; then s=OK; else s=DIFF; fi
    printf '%-42s %-5s exit=%-3s %-4s %s\n' "$f" "$mode" "$code" "$s" "$(grep -v '^velt-units' "$O.err" | grep -m1 -E 'error|ICE|panicked' | cut -c1-90)"
  done
done
