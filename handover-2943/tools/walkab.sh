#!/bin/bash
# walkab.sh <dir> <prog>...: warm rebuild instructions (after a one-line edit) for lazy std,
# units+reach and units+live walk, median of 3.
D=$1; shift
R=/Users/andii/dev/velt/branches/d1-open-pr
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk VELT_THREADS=1
cd "$D" || exit 1
for p in "$@"; do
  for mode in lazy reach walk; do
    case $mode in
      lazy) E=() ;;
      reach) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto) ;;
      walk) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto VELT_UNITS_LIVE_WALK=1) ;;
    esac
    vals=()
    for i in 1 2 3; do
      echo "const z$RANDOM$i = 1;" >> "$p.velt"
      o="$D/$p-$mode-$i-$RANDOM.out"
      log=$(env "${E[@]}" /usr/bin/time -l "$R/target/debug/velt" build "$p.velt" -o "$o" 2>&1 >/dev/null)
      n=$(echo "$log" | awk '/instructions retired/ {print $1}')
      if [ ! -x "$o" ] || echo "$log" | grep -qE "error|panicked|ICE"; then
        echo "FAILED: $p $mode run $i" >&2
        echo "$log" | grep -E "error|panicked|ICE" | head -3 >&2
      fi
      vals+=("$n")
      rm -f "$D/$p-$mode-"*.out
    done
    med=$(printf '%s\n' "${vals[@]}" | sort -n | sed -n 2p)
    printf '%-10s %-6s %6.2f G\n' "$p" "$mode" "$(echo "$med / 1000000000" | bc -l)"
  done
done
