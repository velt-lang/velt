#!/bin/bash
# hello-check-cost.sh: warm `velt check` instructions of hello after a one-line edit (median of
# 3 after a warm-up): lazy, units, units with VELT_UNITS_WALK_STD=0.
R=/Users/andii/dev/velt/branches/d1-open-pr
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab/examples
export VELT_STD=$R/std VELT_THREADS=1
cd "$S" || exit 1
for mode in lazy units nowalk; do
  case $mode in
    lazy) E=() ;;
    units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=auto) ;;
    nowalk) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=auto VELT_UNITS_WALK_STD=0) ;;
  esac
  vals=()
  for i in 1 2 3 4; do
    echo "const zj$RANDOM$i = 1;" >> hello.velt
    log=$(env "${E[@]}" /usr/bin/time -l "$R/target/debug/velt" check hello.velt 2>&1)
    echo "$log" | grep -v '^velt' | grep -qE "error|panicked|ICE" && echo "FAILED: $mode run $i" >&2
    [ "$i" = 1 ] && continue
    vals+=("$(echo "$log" | awk '/instructions retired/ {print $1}')")
  done
  med=$(printf '%s\n' "${vals[@]}" | sort -n | sed -n 2p)
  printf 'check hello %-7s %6.2f G\n' "$mode" "$(echo "$med / 1000000000" | bc -l)"
done
