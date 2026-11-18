#!/bin/bash
# chkmodes.sh <dir> <file>...: velt check per file in lazy, in-process units and disk units (fresh
# unit dir, then warm); prints exit code and first error per mode.
D=$1; shift
R=/Users/andii/dev/velt/branches/d1-open-pr
U=/private/tmp/claude-501/-Users-andii-dev-velt-main/e68817df-6043-420f-9ebf-66ff59acdfc4/scratchpad/u4
O=/private/tmp/claude-501/-Users-andii-dev-velt-main/e68817df-6043-420f-9ebf-66ff59acdfc4/scratchpad/chkmodes.out
export VELT_STD=$R/std
cd "$D" || exit 1
rm -rf "${U:?}"
for f in "$@"; do
  for mode in lazy inproc disk-cold disk-warm; do
    case $mode in
      lazy) E=() ;;
      inproc) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1) ;;
      disk-*) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=$U) ;;
    esac
    env "${E[@]}" "$R/target/debug/velt" check "$f" > "$O" 2>&1
    code=$?
    printf '%-45s %-9s exit=%s %s\n' "$f" "$mode" "$code" "$(grep -v '^velt-units' "$O" | grep -m1 'error' | cut -c1-100)"
  done
done
