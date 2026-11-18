#!/bin/bash
# hello-cost.sh: warm `velt build` instructions of hello after a one-line edit (median of 3
# after a warm-up), lazy vs units vs units with VELT_UNITS_WALK_STD=0; on the ab7.sh copy.
R=/Users/andii/dev/velt/branches/d1-open-pr
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab/examples
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$S" || exit 1
for mode in lazy units nowalk; do
  case $mode in
    lazy) E=() ;;
    units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto) ;;
    nowalk) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto VELT_UNITS_WALK_STD=0) ;;
  esac
  vals=()
  for i in 1 2 3 4; do
    echo "const zh$RANDOM$i = 1;" >> hello.velt
    o="$S/hello-$mode-$i-$RANDOM.out"
    log=$(env "${E[@]}" /usr/bin/time -l "$R/target/debug/velt" build hello.velt -o "$o" 2>&1 >/dev/null)
    if [ ! -x "$o" ] || echo "$log" | grep -v '^velt' | grep -qE "error|panicked|ICE"; then
      echo "FAILED: $mode run $i" >&2
    fi
    rm -f "$o"
    [ "$i" = 1 ] && continue
    vals+=("$(echo "$log" | awk '/instructions retired/ {print $1}')")
  done
  med=$(printf '%s\n' "${vals[@]}" | sort -n | sed -n 2p)
  printf 'build hello %-7s %6.2f G\n' "$mode" "$(echo "$med / 1000000000" | bc -l)"
done
