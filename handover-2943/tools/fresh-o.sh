#!/bin/bash
# fresh-o.sh <velt>: instructions of warm hello builds, lazy std and from std's unit on disk,
# to the same output path every time vs to a fresh one each time (ab7.sh's way), median of 3
# after a warm-up, VELT_THREADS=1; the edit between builds is a comment.
V=$1
R=/Users/andii/dev/velt/branches/d1-open-pr
D=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/fresh-o
rm -rf "$D" && mkdir -p "$D/u"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk VELT_STD=$R/std
export VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
cp "$R/tests/golden/m1/hello.velt" "$D/h.velt"
for mode in lazy units; do
  case $mode in
    lazy) E=() ;;
    units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK="$D/u") ;;
  esac
  for out in same fresh; do
    vals=()
    for i in 1 2 3 4; do
      echo "// $mode $out $i $RANDOM" >> "$D/h.velt"
      case $out in
        same) o="$D/o-$mode" ;;
        fresh) o="$D/o-$mode-$i-$RANDOM" ;;
      esac
      n=$(env "${E[@]}" /usr/bin/time -l "$V" build "$D/h.velt" -o "$o" 2>&1 >/dev/null \
        | awk '/instructions retired/ {print $1}')
      [ "$i" = 1 ] || vals+=("$n")
    done
    med=$(printf '%s\n' "${vals[@]}" | sort -n | sed -n 2p)
    printf '%-6s %-6s %6.3f G\n' "$mode" "$out" "$(echo "$med / 1000000000" | bc -l)"
  done
done
