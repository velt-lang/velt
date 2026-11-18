#!/bin/bash
# ab2.sh <base-velt> <head-velt> <prog>...: interleaved A/B of two compilers on warm builds after
# a comment edit, lazy std and units: instructions and wall time, median of 5 (bash 3.2).
R=/Users/andii/dev/velt/branches/d1-open-pr
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab/examples
A=$1; B=$2; shift 2
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$S" || exit 1
med() { tr ' ' '\n' | grep -v '^$' | sort -n | sed -n 3p; }
for p in "$@"; do
  cp "$p.velt" "$p.velt.keep"
  for mode in lazy units; do
    case $mode in
      lazy) E=() ;;
      units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto) ;;
    esac
    # Warm both: the unit on disk and each compiler's cache entries.
    for v in "$A" "$B" "$A" "$B"; do echo "// w$RANDOM" >> "$p.velt"; env "${E[@]}" "$v" build "$p.velt" -o "$S/ab2w" > /dev/null 2>&1; done
    ia=""; ib=""; ta=""; tb=""
    for i in 1 2 3 4 5; do
      for side in a b; do
        v=$A; [ $side = b ] && v=$B
        echo "// r$RANDOM$i" >> "$p.velt"
        log=$(env "${E[@]}" /usr/bin/time -l "$v" build "$p.velt" -o "$S/ab2-$side" --timings 2>&1)
        [ -x "$S/ab2-$side" ] || echo "FAILED: $p $mode $side run $i" >&2
        n=$(echo "$log" | awk '/instructions retired/ {print $1}')
        t=$(echo "$log" | awk '/^velt: total/ {print $3}')
        case $side in a) ia="$ia $n"; ta="$ta $t" ;; b) ib="$ib $n"; tb="$tb $t" ;; esac
      done
    done
    printf '%-9s %-6s base %6.3f G %8s ms   head %6.3f G %8s ms\n' "$p" "$mode" \
      "$(echo "$(echo $ia | med) / 1000000000" | bc -l)" "$(echo $ta | med)" \
      "$(echo "$(echo $ib | med) / 1000000000" | bc -l)" "$(echo $tb | med)"
  done
  cp "$p.velt.keep" "$p.velt"
done
rm -f "$S"/ab2w "$S"/ab2-a "$S"/ab2-b
