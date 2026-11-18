#!/bin/bash
# checkab.sh <dir> <prog>...: warm `velt check` instructions after a one-line edit, lazy vs units
# from disk vs units from disk with VELT_UNITS_WALK_STD=1; median of 3; flags failed checks.
D=$1; shift
R=/Users/andii/dev/velt/branches/d1-open-pr
export VELT_STD=$R/std VELT_THREADS=1
cd "$D" || exit 1
for p in "$@"; do
  for mode in lazy disk walk; do
    case $mode in
      lazy) E=() ;;
      disk) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=auto) ;;
      walk) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_DISK=auto VELT_UNITS_WALK_STD=1) ;;
    esac
    vals=()
    for i in 1 2 3 4; do
      echo "const z$RANDOM$i = 1;" >> "$p.velt"
      log=$(env "${E[@]}" /usr/bin/time -l "$R/target/debug/velt" check "$p.velt" 2>&1)
      if echo "$log" | grep -v '^velt' | grep -qE "error|panicked|ICE"; then
        echo "FAILED: $p $mode run $i" >&2
      fi
      [ "$i" = 1 ] && continue
      vals+=("$(echo "$log" | awk '/instructions retired/ {print $1}')")
    done
    med=$(printf '%s\n' "${vals[@]}" | sort -n | sed -n 2p)
    printf '%-10s %-5s %6.2f G\n' "$p" "$mode" "$(echo "$med / 1000000000" | bc -l)"
  done
done
