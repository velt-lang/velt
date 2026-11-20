#!/bin/bash
# In the OrbStack VM (velt-prof): one release velt + runtime from sleep-home, then the timer rows
# built with the base std (main-base worktree) and the head std (sleep-home), instructions by
# cachegrind and a per-function profile by callgrind for one row.
#   orb -m velt-prof bash vm-cg.sh build | rows | profile <row> | clean
set -u
source "$HOME/.cargo/env"
OUT=$HOME/r3273
HEAD=/Users/andii/dev/velt/branches/sleep-home
BASE=/Users/andii/dev/velt/branches/main-base
V=$OUT/t/release/velt
mkdir -p "$OUT/bin" "$OUT/cg"
build_row() { # <row> <side> <std>
  VELT_CODEGEN_UNITS=1 VELT_STD=$3 VELT_RT_LIB=$OUT/t/release/libvelt_rt.a "$V" build --release -g "$HEAD/bench/async/$1.velt" -o "$OUT/bin/$1-$2" > "$OUT/bin/$1-$2.log" 2>&1 || { echo "build $1 $2 failed"; tail -3 "$OUT/bin/$1-$2.log"; }
}
count() { # <exe> <threads>: instructions (cachegrind)
  VELT_THREADS=$2 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$1" 2>&1 > /dev/null | awk '/I *refs:/ {gsub(",", "", $NF); print $NF}'
}
case "${1:-}" in
build)
  cd "$HEAD" || exit 1
  CARGO_TARGET_DIR=$OUT/t CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 cargo build --release -q -p veltc -p velt_rt 2>&1 | tail -3
  ls -la "$V" "$OUT/t/release/libvelt_rt.a" | cut -c30-
  ;;
rows)
  shift
  for row in "${@:-timers_due_join timers_due_stored timers_due_reversed timers_due_in_promise timers timers_fire}"; do
    for r in $row; do
      build_row "$r" base "$BASE/std"; build_row "$r" head "$HEAD/std"
      for th in 1 4; do
        b=$(count "$OUT/bin/$r-base" $th); h=$(count "$OUT/bin/$r-head" $th)
        echo "$r T=$th: base $b head $h $(echo "scale=2; ($h - $b) * 100 / $b" | bc)% ($(( (h - b) / 1000 ))k)"
      done
    done
  done
  ;;
ab)
  # Every async row (or the ones given): base std vs the std tree in $AB_STD, T=1 and T=4, in
  # basis points.   AB_STD=<std> bash vm-cg.sh ab [row...]
  shift
  rows=("$@")
  if [ ${#rows[@]} = 0 ]; then
    for f in "$HEAD"/bench/async/*.velt; do rows+=("$(basename "$f" .velt)"); done
  fi
  for r in "${rows[@]}"; do
    build_row "$r" base "$BASE/std"; build_row "$r" ab "$AB_STD"
    for th in 1 4; do
      b=$(count "$OUT/bin/$r-base" $th); h=$(count "$OUT/bin/$r-ab" $th)
      [ -n "$b" ] && [ -n "$h" ] && echo "$r T=$th: base $b head $h $(( (h - b) * 10000 / b ))bp"
    done
  done
  ;;
profab)
  # callgrind of one row, base std vs $AB_STD, per function (inlined source lines summed), the
  # differences largest first.   AB_STD=<std> bash vm-cg.sh profab <row>
  row=$2
  build_row "$row" base "$BASE/std"; build_row "$row" ab "$AB_STD"
  for side in base ab; do
    VELT_THREADS=${TH:-1} valgrind --tool=callgrind --callgrind-out-file="$OUT/cg/$row-$side.out" "$OUT/bin/$row-$side" > /dev/null 2> /dev/null
    callgrind_annotate --threshold=100 "$OUT/cg/$row-$side.out" 2>/dev/null | grep -E "^ *[0-9,]+ \(" |
      sed -E 's/ \[[^]]*\]$//' | awk '{n=$1; gsub(",", "", n); f=$NF; sub(/^[^:]*:/, "", f); sub(/\x27[0-9]+$/, "", f); t[f]+=n} END{for (f in t) print t[f], f}' | sort -k2 > "$OUT/cg/$row-$side.fn"
  done
  join -1 2 -2 2 -a 1 -a 2 -e 0 -o 0,1.1,2.1 "$OUT/cg/$row-base.fn" "$OUT/cg/$row-ab.fn" | awk '{d=$3-$2; if (d>40000 || d<-40000) printf "%+9d  %9d -> %9d  %s\n", d, $2, $3, $1}' | sort -rn | head -25 | cut -c1-140
  ;;
annot)
  # callgrind of one row, base std vs $AB_STD, the whole line-annotated output of each side
  # written where the Mac reads it (notes-3097/ann/<row>-<side>.txt).
  #   AB_STD=<std> bash vm-cg.sh annot <row>
  row=$2
  A=/Users/andii/dev/velt/branches/notes-3097/ann; mkdir -p "$A"
  build_row "$row" base "$BASE/std"; build_row "$row" ab "$AB_STD"
  for side in base ab; do
    VELT_THREADS=${TH:-1} valgrind --tool=callgrind --dump-instr=no --callgrind-out-file="$OUT/cg/$row-$side.out" "$OUT/bin/$row-$side" > /dev/null 2> /dev/null
    callgrind_annotate --auto=yes --show-percs=no --threshold=100 "$OUT/cg/$row-$side.out" > "$A/$row-$side.txt" 2>&1
  done
  ls -la "$A" | tail -3
  ;;
rep)
  # One row, base std vs $AB_STD, N runs each at T threads interleaved: every count, to see
  # whether a bimodal row's high mode shows on one side only.
  #   AB_STD=<std> bash vm-cg.sh rep <row> <threads> <runs>
  row=$2; th=$3; n=$4
  build_row "$row" base "$BASE/std"; build_row "$row" ab "$AB_STD"
  for i in $(seq 1 "$n"); do
    echo "$row T=$th run $i: base $(count "$OUT/bin/$row-base" "$th") head $(count "$OUT/bin/$row-ab" "$th")"
  done
  ;;
trio)
  # Rows with the base, the pushed head (notes-3097/var/pushed/std, from git archive) and the
  # working tree, each against the base, in basis points; T=1 and T=4.
  shift
  P=/Users/andii/dev/velt/branches/notes-3097/var/pushed/std
  for r in ${@:-timers_due_join timers_due_stored timers_due_reversed timers_due_in_promise timers timers_fire}; do
    build_row "$r" base "$BASE/std"; build_row "$r" pushed "$P"; build_row "$r" head "$HEAD/std"
    for th in 1 4; do
      b=$(count "$OUT/bin/$r-base" $th); p=$(count "$OUT/bin/$r-pushed" $th); h=$(count "$OUT/bin/$r-head" $th)
      echo "$r T=$th: base $b  pushed +$(( (p - b) / 1000 ))k ($(( (p - b) * 10000 / b ))bp)  head +$(( (h - b) / 1000 ))k ($(( (h - b) * 10000 / b ))bp)"
    done
  done
  ;;
vars)
  # Rows built with the base, the head and each measurement variant (notes-3097/var/<v>/std,
  # made by mkvar.py): instructions at one worker, and each variant's saving against the head.
  shift
  VAR=/Users/andii/dev/velt/branches/notes-3097/var
  for r in "${@:-timers_due_join timers_due_stored}"; do
    for r1 in $r; do
      build_row "$r1" base "$BASE/std"; build_row "$r1" head "$HEAD/std"
      b=$(count "$OUT/bin/$r1-base" 1); h=$(count "$OUT/bin/$r1-head" 1)
      echo "$r1 T=1: base $b head +$(( (h - b) / 1000 ))k ($(( (h - b) * 10000 / b ))bp)"
      for v in $(ls "$VAR"); do
        build_row "$r1" "v-$v" "$VAR/$v/std"
        n=$(count "$OUT/bin/$r1-v-$v" 1)
        echo "  $v: +$(( (n - b) / 1000 ))k, saves $(( (h - n) / 1000 ))k"
      done
    done
  done
  ;;
profile)
  row=$2
  for side in base head; do
    std=$BASE/std; [ $side = head ] && std=$HEAD/std
    build_row "$row" $side "$std"
    VELT_THREADS=1 valgrind --tool=callgrind --callgrind-out-file="$OUT/cg/$row-$side.out" "$OUT/bin/$row-$side" > /dev/null 2> "$OUT/cg/$row-$side.err"
    callgrind_annotate --threshold=100 "$OUT/cg/$row-$side.out" 2>/dev/null | grep -E "^ *[0-9,]+ " | head -70 | sed -E "s/ \[[^]]*\]$//; s/_V3stdP4langP5hooksP[0-9]+//" > "$OUT/cg/$row-$side.txt"
  done
  cp "$OUT"/cg/$row-*.txt /Users/andii/dev/velt/branches/notes-3097/notes/ 2>/dev/null
  echo "profiles in $OUT/cg"
  ;;
clean)
  rm -rf "$OUT/t" "$OUT/bin"
  sudo fstrim -a 2>/dev/null
  df -h "$HOME" | tail -1
  ;;
esac
