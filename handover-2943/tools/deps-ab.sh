#!/bin/bash
# deps-ab.sh <velt> <app dir with main.velt>: instructions of warm `velt check` and `velt build`
# (a comment edit between runs, the same output path, VELT_THREADS=1), median of 3 after a
# warm-up, in three modes: lazy std; std from its unit on disk; std and the dependencies from
# units (VELT_UNITS_LIB=1, library units). Flags runs that fail.
V=$1
A=$2
R=/Users/andii/dev/velt/branches/d1-open-pr
D=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/deps-ab
rm -rf "$D" && mkdir -p "$D"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk VELT_STD=$R/std
export VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
cd "$A" || exit 1
cp main.velt "$D/main.velt.orig"
for cmd in check build; do
  for mode in lazy units libs; do
    case $mode in
      lazy) E=() ;;
      units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK="$D/u-$mode") ;;
      libs) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_LIB=1 VELT_UNITS_DISK="$D/u-$mode") ;;
    esac
    mkdir -p "$D/u-$mode"
    vals=()
    for i in 1 2 3 4; do
      echo "// $cmd $mode $i $RANDOM" >> main.velt
      case $cmd in
        check) log=$(env "${E[@]}" /usr/bin/time -l "$V" check main.velt 2>&1) ;;
        build) log=$(env "${E[@]}" /usr/bin/time -l "$V" build main.velt -o "$D/o-$mode" 2>&1) ;;
      esac
      if echo "$log" | grep -v '^velt' | grep -qE "^error|panicked|ICE"; then
        echo "FAILED: $cmd $mode run $i" >&2
        echo "$log" | grep -E "error|panicked|ICE" | head -3 >&2
      fi
      [ "$i" = 1 ] && continue
      vals+=("$(echo "$log" | awk '/instructions retired/ {print $1}')")
    done
    med=$(printf '%s\n' "${vals[@]}" | sort -n | sed -n 2p)
    printf '%-6s %-6s %7.3f G\n' "$cmd" "$mode" "$(echo "$med / 1000000000" | bc -l)"
  done
done
cp "$D/main.velt.orig" main.velt
