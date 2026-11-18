#!/bin/bash
# pmap-ab.sh: interleaved A/B of the prelude_def map. Builds velt without it (the three
# velt_vir files set aside and checked out from HEAD, then put back), keeps both binaries, and
# runs warm todo_api builds alternating them: lower time (median of 5) and instructions.
R=/Users/andii/dev/velt/branches/d1-open-pr
P=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/pmap
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab/examples
FILES=(crates/velt_vir/src/lower/array_view.rs crates/velt_vir/src/lower/mod.rs crates/velt_vir/src/lower/program.rs)
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
mkdir -p "$P"
cd "$R" || exit 1
cp target/debug/velt "$P/velt-map"
for f in "${FILES[@]}"; do cp "$f" "$P/$(basename "$f")"; done
git checkout -q HEAD -- "${FILES[@]}"
cargo build -q -p veltc --bin velt || { for f in "${FILES[@]}"; do cp "$P/$(basename "$f")" "$f"; done; exit 1; }
cp target/debug/velt "$P/velt-scan"
for f in "${FILES[@]}"; do cp "$P/$(basename "$f")" "$f"; done
cargo build -q -p veltc --bin velt || exit 1
git status --short | grep -v '^??'
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$S" || exit 1
cp todo_api.velt todo_api.velt.keep
for mode in lazy units; do
  case $mode in
    lazy) E=() ;;
    units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto) ;;
  esac
  for b in scan map; do echo "// w$RANDOM" >> todo_api.velt; env "${E[@]}" "$P/velt-$b" build todo_api.velt -o "$S/pw" > /dev/null 2>&1; done
  declare -A low ins
  low=(); ins=()
  for i in 1 2 3 4 5; do
    for b in scan map; do
      echo "// r$RANDOM$i" >> todo_api.velt
      log=$(env "${E[@]}" /usr/bin/time -l "$P/velt-$b" build todo_api.velt -o "$S/pr-$b" --timings 2>&1)
      low[$b]+="$(echo "$log" | awk '/^velt: lower/ {print $3}') "
      ins[$b]+="$(echo "$log" | awk '/instructions retired/ {print $1}') "
    done
  done
  for b in scan map; do
    ml=$(echo ${low[$b]} | tr ' ' '\n' | sort -n | sed -n 3p)
    mi=$(echo ${ins[$b]} | tr ' ' '\n' | sort -n | sed -n 3p)
    printf '%-6s %-5s lower %8s ms   %6.2f G\n' "$mode" "$b" "$ml" "$(echo "$mi / 1000000000" | bc -l)"
  done
  unset low ins
done
cp todo_api.velt.keep todo_api.velt
rm -f "$S"/pw "$S"/pr-*
