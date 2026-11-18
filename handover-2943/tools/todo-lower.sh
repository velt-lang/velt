#!/bin/bash
# todo-lower.sh: warm `velt build` of todo_api after a comment edit, lazy std and units: the
# lower phase's time and instructions retired (median of 3 after a warm-up).
R=/Users/andii/dev/velt/branches/d1-open-pr
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab/examples
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$S" || exit 1
cp todo_api.velt todo_api.velt.keep
for mode in lazy units; do
  case $mode in
    lazy) E=() ;;
    units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto) ;;
  esac
  ins=(); low=()
  for i in 1 2 3 4; do
    echo "// l$RANDOM$i" >> todo_api.velt
    o="$S/tl-$mode-$i-$RANDOM"
    log=$(env "${E[@]}" /usr/bin/time -l "$R/target/debug/velt" build todo_api.velt -o "$o" --timings 2>&1)
    [ -x "$o" ] || echo "FAILED: $mode run $i" >&2
    rm -f "$o"
    [ "$i" = 1 ] && continue
    ins+=("$(echo "$log" | awk '/instructions retired/ {print $1}')")
    low+=("$(echo "$log" | awk '/^velt: lower/ {print $3}')")
  done
  mi=$(printf '%s\n' "${ins[@]}" | sort -n | sed -n 2p)
  ml=$(printf '%s\n' "${low[@]}" | sort -n | sed -n 2p)
  printf 'build todo_api %-6s %6.2f G   lower %s ms\n' "$mode" "$(echo "$mi / 1000000000" | bc -l)" "$ml"
done
cp todo_api.velt.keep todo_api.velt
