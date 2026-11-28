#!/usr/bin/env bash
# Instructions (cachegrind) of the rows copies made with the receiver's own class touch (#3817),
# with two trees' debug-profile velt and std:
#
#   bench/compile/species_ab.sh BASE_TREE HEAD_TREE [WORK]
#
# Each tree must have `target/debug/velt` and the runtime built (`cargo build -p veltc --bin velt
# -p velt_rt -p velt_rt_shared`). Build rows (the `velt` process only: the linker is not
# followed): hello, the 10k-line application of the build-speed yardstick (app.py), and programs
# without a subclass that loop over regex `split`/`matchAll`, typed array
# `slice`/`map`/`filter`/`subarray` and array `map`/`filter`/`slice`. Run rows: those three
# programs, built with `--release`. Prints `row base head change`.
set -euo pipefail
BASE=${1:?base tree}
HEAD=${2:?head tree}
W=${3:-${TMPDIR:-/tmp}/species-ab}
mkdir -p "$W/app" "$W/cg" "$W/tc-base" "$W/tc-head"
# Copies, so a build in either tree during the run doesn't change what is measured.
for side in base head; do
  tree=$BASE
  [[ $side == head ]] && tree=$HEAD
  cp "$tree/target/debug/velt" "$tree"/target/debug/libvelt_rt.a "$tree"/target/debug/libvelt_rt_shared.* "$W/tc-$side/"
done
python3 "$HEAD/bench/compile/scale/app.py" 10000 "$W/app" > /dev/null
cat > "$W/regex_loop.velt" <<'VELT'
function main() {
  const re = /,\s*/g;
  const s = "a, b,c,  d, e, f, g, h";
  let n = 0;
  for (let i = 0; i < 2000; i++) {
    n += s.split(re).length;
    for (const m of s.matchAll(re)) {
      n += m.index;
    }
  }
  console.log(n);
}
VELT
cat > "$W/typed_loop.velt" <<'VELT'
function main() {
  const xs = new Int32Array(64);
  for (let i = 0; i < 64; i++) {
    xs[i] = i;
  }
  const fs = new Float64Array([1.5, 2.5, 3.5, 4.5]);
  let n = 0;
  for (let i = 0; i < 2000; i++) {
    n += xs.slice(1, 40).length + xs.map((x) => x + 1)[3] + xs.filter((x) => x > 30).length;
    n += xs.subarray(5).length + fs.slice(1).length + fs.map((x) => x * 2)[0];
  }
  console.log(n);
}
VELT
cat > "$W/array_loop.velt" <<'VELT'
function main() {
  const xs: number[] = [];
  for (let i = 0; i < 64; i++) {
    xs.push(i);
  }
  let n = 0;
  for (let i = 0; i < 2000; i++) {
    n += xs.map((x) => x + 1)[3] + xs.filter((x) => x > 30).length + xs.slice(2).length;
  }
  console.log(n);
}
VELT
export VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off
build() { # side tree row args...
  local side=$1 tree=$2 row=$3
  shift 3
  local home="$W/home-$side-$row"
  rm -rf "$home"
  mkdir -p "$home"
  (cd "$W" && VELT_HOME=$home VELT_STD=$tree/std valgrind --tool=cachegrind --cache-sim=no \
    --cachegrind-out-file="$W/cg/$side-$row.out" "$W/tc-$side/velt" "$@" > /dev/null 2>&1) ||
    echo "$side $row: velt failed" >&2
  awk '/^summary:/ { print $2 }' "$W/cg/$side-$row.out"
}
run() { # side row exe
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$W/cg/run-$1-$2.out" "$3" > /dev/null 2>&1
  awk '/^summary:/ { print $2 }' "$W/cg/run-$1-$2.out"
}
row() {
  awk -v r="$1" -v b="$2" -v h="$3" 'BEGIN { printf "%-18s %14d %14d %+7.2f%%\n", r, b, h, (h - b) * 100 / b }'
}
for name in hello app10k regex_loop typed_loop array_loop; do
  for side in base head; do
    tree=$BASE
    [[ $side == head ]] && tree=$HEAD
    case $name in
      hello) src="$tree/tests/golden/m1/hello.velt" ;;
      app10k) src="$W/app/main.velt" ;;
      *) src="$W/$name.velt" ;;
    esac
    n=$(build $side "$tree" "$name" build "$src" -o "$W/$name-$side")
    eval "${side}_b=$n"
  done
  row "$name build" "$base_b" "$head_b"
done
for name in regex_loop typed_loop array_loop; do
  for side in base head; do
    tree=$BASE
    [[ $side == head ]] && tree=$HEAD
    build $side "$tree" "$name-rel" build --release "$W/$name.velt" -o "$W/$name-rel-$side" > /dev/null
    n=$(run $side "$name" "$W/$name-rel-$side")
    eval "${side}_r=$n"
  done
  row "$name run" "$base_r" "$head_r"
done
