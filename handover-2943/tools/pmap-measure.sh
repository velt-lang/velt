#!/bin/bash
# pmap-measure.sh: the measuring half of pmap-ab.sh with the binaries it built (bash 3.2: no
# associative arrays). Warm todo_api builds alternating velt-scan and velt-map: lower time and
# instructions, median of 5, lazy std and units.
R=/Users/andii/dev/velt/branches/d1-open-pr
P=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/pmap
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab/examples
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$S" || exit 1
cp todo_api.velt todo_api.velt.keep
med() { tr ' ' '\n' | grep -v '^$' | sort -n | sed -n 3p; }
for mode in lazy units; do
  case $mode in
    lazy) E=() ;;
    units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto) ;;
  esac
  for b in scan map; do echo "// w$RANDOM" >> todo_api.velt; env "${E[@]}" "$P/velt-$b" build todo_api.velt -o "$S/pw" > /dev/null 2>&1; done
  low_scan=""; low_map=""; ins_scan=""; ins_map=""
  for i in 1 2 3 4 5; do
    for b in scan map; do
      echo "// r$RANDOM$i" >> todo_api.velt
      log=$(env "${E[@]}" /usr/bin/time -l "$P/velt-$b" build todo_api.velt -o "$S/pr-$b" --timings 2>&1)
      l=$(echo "$log" | awk '/^velt: lower/ {print $3}')
      n=$(echo "$log" | awk '/instructions retired/ {print $1}')
      case $b in
        scan) low_scan="$low_scan $l"; ins_scan="$ins_scan $n" ;;
        map) low_map="$low_map $l"; ins_map="$ins_map $n" ;;
      esac
    done
  done
  printf '%-6s scan  lower %8s ms   %6.3f G\n' "$mode" "$(echo $low_scan | med)" "$(echo "$(echo $ins_scan | med) / 1000000000" | bc -l)"
  printf '%-6s map   lower %8s ms   %6.3f G\n' "$mode" "$(echo $low_map | med)" "$(echo "$(echo $ins_map | med) / 1000000000" | bc -l)"
done
cp todo_api.velt.keep todo_api.velt
rm -f "$S"/pw "$S"/pr-*
