#!/bin/bash
# In the OrbStack VM (velt-prof, arm64 Linux): a release velt + runtime built from the let-go-knobs
# worktree, then each row built with the base std (eb9c6e769, #3583's head) and the head std (the
# worktree's), counted by cachegrind at T1 and T4. One compiler: only std differs.
#   orb -m velt-prof bash vmrows.sh build | rows "<rows>" | clean
set -u
source "$HOME/.cargo/env"
OUT=$HOME/s13
TREE=/Users/andii/dev/velt/branches/let-go-knobs
BASE_STD=/Users/andii/dev/velt/branches/s13-work/base/std
V=$OUT/t/release/velt
mkdir -p "$OUT/bin"
stdof() { if [ "$1" = base ]; then echo "$BASE_STD"; else echo "$TREE/std"; fi; }
build_row() { # <row> <side>
  VELT_CODEGEN_UNITS=1 VELT_STD=$(stdof "$2") VELT_RT_LIB=$OUT/t/release/libvelt_rt.a "$V" build --release -g "$TREE/bench/async/$1.velt" -o "$OUT/bin/$1-$2" > "$OUT/bin/$1-$2.log" 2>&1 || { echo "build $1 $2 failed"; tail -3 "$OUT/bin/$1-$2.log"; }
}
count() { # <exe> <threads>
  VELT_THREADS=$2 timeout 900 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$1" 2>&1 > /dev/null | awk '/I *refs:/ {gsub(",", "", $NF); print $NF}'
}
case "${1:-}" in
build)
  cd "$TREE" || exit 1
  CARGO_TARGET_DIR=$OUT/t CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 nice -n 19 cargo build --release -q -p veltc -p velt_rt 2>&1 | tail -3
  ls -la "$V" "$OUT/t/release/libvelt_rt.a" | cut -c30-
  ;;
rows)
  for r in $2; do
    build_row "$r" base
    build_row "$r" head
    for th in 1 4; do
      b=$(count "$OUT/bin/$r-base" $th); h=$(count "$OUT/bin/$r-head" $th)
      if [ $th = 4 ]; then
        # T4 moves with the interleaving: median of three each.
        b2=$(count "$OUT/bin/$r-base" 4); h2=$(count "$OUT/bin/$r-head" 4)
        b3=$(count "$OUT/bin/$r-base" 4); h3=$(count "$OUT/bin/$r-head" 4)
        b=$(printf "%s\n" $b $b2 $b3 | sort -n | sed -n 2p); h=$(printf "%s\n" $h $h2 $h3 | sort -n | sed -n 2p)
      fi
      echo "$r T$th base $b head $h  $(awk -v b=$b -v h=$h 'BEGIN { printf "%+.2f%%", (h - b) * 100 / b }')"
    done
  done
  ;;
prof)
  # callgrind of one row at T1, base vs head, per function, the differences largest first.
  row=$2
  mkdir -p "$OUT/cg"
  for side in base head; do
    build_row "$row" "$side"
    VELT_THREADS=${TH:-1} valgrind --tool=callgrind --callgrind-out-file="$OUT/cg/$row-$side.out" "$OUT/bin/$row-$side" > /dev/null 2> /dev/null
    callgrind_annotate --threshold=100 "$OUT/cg/$row-$side.out" 2>/dev/null | grep -E "^ *[0-9,]+ \(" |
      sed -E 's/ \[[^]]*\]$//' | awk '{n=$1; gsub(",", "", n); f=$NF; sub(/^[^:]*:/, "", f); t[f]+=n} END{for (f in t) print t[f], f}' | sort -k2 > "$OUT/cg/$row-$side.fn"
  done
  join -1 2 -2 2 -a 1 -a 2 -e 0 -o 0,1.1,2.1 "$OUT/cg/$row-base.fn" "$OUT/cg/$row-head.fn" | awk '{d=$3-$2; if (d>20000 || d<-20000) printf "%+11d  %11d -> %11d  %s\n", d, $2, $3, $1}' | sort -rn | head -30 | cut -c1-160
  ;;
clean)
  /bin/rm -rf -- "$OUT"
  ;;
esac
