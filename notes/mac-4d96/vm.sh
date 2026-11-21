#!/bin/bash
# In the OrbStack VM (velt-prof), after mac-60d0's vm-cg.sh: a release velt + runtime from
# sleep-home, then rows built with main-base's std and each variant's std (sleep-combo-var/<v>/std):
# cachegrind instructions at T1 and T4, each variant against the base in basis points.
#   orb -m velt-prof bash vm.sh build | vars "<rows>" <variant...> | prof <row> <a> <b> | clean
set -u
source "$HOME/.cargo/env"
OUT=$HOME/r135
B=/Users/andii/dev/velt/branches
HEAD=$B/sleep-home
BASE=$B/main-base
VAR=$B/sleep-combo-var
V=$OUT/t/release/velt
mkdir -p "$OUT/bin" "$OUT/cg"
stdof() { if [ "$1" = base ]; then echo "$BASE/std"; else echo "$VAR/$1/std"; fi; }
build_row() { # <row> <variant>
  VELT_CODEGEN_UNITS=1 VELT_STD=$(stdof "$2") VELT_RT_LIB=$OUT/t/release/libvelt_rt.a "$V" build --release -g "$HEAD/bench/async/$1.velt" -o "$OUT/bin/$1-$2" > "$OUT/bin/$1-$2.log" 2>&1 || { echo "build $1 $2 failed"; tail -3 "$OUT/bin/$1-$2.log"; }
}
count() { # <exe> <threads>
  VELT_THREADS=$2 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$1" 2>&1 > /dev/null | awk '/I *refs:/ {gsub(",", "", $NF); print $NF}'
}
case "${1:-}" in
build)
  cd "${SRC:-$HEAD}" || exit 1
  CARGO_TARGET_DIR=$OUT/t CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 cargo build --release -q -p veltc -p velt_rt 2>&1 | tail -3
  ls -la "$V" "$OUT/t/release/libvelt_rt.a" | cut -c30-
  ;;
vars)
  rows=$2; shift 2
  for r in $rows; do
    build_row "$r" base
    b1=$(count "$OUT/bin/$r-base" 1); b4=$(count "$OUT/bin/$r-base" 4)
    echo "$r base: T1 $b1 T4 $b4"
    for v in "$@"; do
      build_row "$r" "$v"
      n1=$(count "$OUT/bin/$r-$v" 1); n4=$(count "$OUT/bin/$r-$v" 4)
      echo "  $v: T1 $(( (n1 - b1) * 10000 / b1 ))bp ($(( (n1 - b1) / 1000 ))k)  T4 $(( (n4 - b4) * 10000 / b4 ))bp ($(( (n4 - b4) / 1000 ))k)"
    done
  done
  ;;
prof)
  # callgrind of one row, variant <a> vs <b>, per function, the differences largest first.
  row=$2; a=$3; b=$4
  for side in $a $b; do
    build_row "$row" "$side"
    VELT_THREADS=${TH:-1} valgrind --tool=callgrind --callgrind-out-file="$OUT/cg/$row-$side.out" "$OUT/bin/$row-$side" > /dev/null 2> /dev/null
    callgrind_annotate --threshold=100 "$OUT/cg/$row-$side.out" 2>/dev/null | grep -E "^ *[0-9,]+ \(" |
      sed -E 's/ \[[^]]*\]$//' | awk '{n=$1; gsub(",", "", n); f=$NF; sub(/^[^:]*:/, "", f); sub(/\x27[0-9]+$/, "", f); t[f]+=n} END{for (f in t) print t[f], f}' | sort -k2 > "$OUT/cg/$row-$side.fn"
  done
  join -1 2 -2 2 -a 1 -a 2 -e 0 -o 0,1.1,2.1 "$OUT/cg/$row-$a.fn" "$OUT/cg/$row-$b.fn" | awk '{d=$3-$2; if (d>20000 || d<-20000) printf "%+9d  %9d -> %9d  %s\n", d, $2, $3, $1}' | sort -rn | head -40 | cut -c1-150
  ;;
annot)
  # the line-annotated callgrind output of one row and variant, written beside the variants.
  row=$2; side=$3
  build_row "$row" "$side"
  VELT_THREADS=${TH:-1} valgrind --tool=callgrind --dump-instr=no --callgrind-out-file="$OUT/cg/$row-$side.out" "$OUT/bin/$row-$side" > /dev/null 2> /dev/null
  callgrind_annotate --auto=yes --show-percs=no --threshold=100 "$OUT/cg/$row-$side.out" > "$VAR/ann-$row-$side.txt" 2>&1
  ls -la "$VAR/ann-$row-$side.txt"
  ;;
reps)
  # One row at <threads>, <n> interleaved runs of the base and each variant: the minimum and the
  # median of each, and both against the base's.   vm.sh reps <row> <threads> <n> <variant...>
  row=$2; th=$3; n=$4; shift 4
  for v in base "$@"; do build_row "$row" "$v"; : > "$OUT/cg/reps-$row-$v.txt"; done
  for i in $(seq 1 "$n"); do
    for v in base "$@"; do count "$OUT/bin/$row-$v" "$th" >> "$OUT/cg/reps-$row-$v.txt"; done
  done
  stat() { sort -n "$1" | awk '{a[NR]=$1} END {printf "%d %d", a[1], a[int((NR+1)/2)]}'; }
  read bmin bmed <<< "$(stat "$OUT/cg/reps-$row-base.txt")"
  echo "$row T$th, $n runs: base min $bmin median $bmed"
  for v in "$@"; do
    read vmin vmed <<< "$(stat "$OUT/cg/reps-$row-$v.txt")"
    echo "  $v: min $(( (vmin - bmin) * 10000 / bmin ))bp  median $(( (vmed - bmed) * 10000 / bmed ))bp  (min $vmin, median $vmed)"
  done
  ;;
clean)
  cargo clean --target-dir "$OUT/t" 2>/dev/null | tail -1
  find "$OUT" -type f -delete
  sudo fstrim -a 2>/dev/null
  df -h "$HOME" | tail -1
  ;;
esac
