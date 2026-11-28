#!/usr/bin/env bash
# Instructions (cachegrind, velt only: the linker is not followed) of the compiler rows the
# `performance` global touches (#2739), with two trees' debug-profile velt and std:
#
#   bench/compile/perf_global_ab.sh BASE_TREE HEAD_TREE [WORK]
#
# Each tree must have `target/debug/velt` built (`cargo build -p veltc --bin velt -p velt_rt
# -p velt_rt_shared`). Rows: `velt build` of hello, of a program that only calls
# `performance.now()` (which must not load the global), of the 10k-line application of the
# build-speed yardstick (app.py), and `velt check` of every std module (all_std.velt).
# Prints `row base head change`. `velt check` of all_std.velt stops at a generator error on
# Linux (`generators need generated code to be built, in a cycle`) on both sides alike.
set -euo pipefail
BASE=${1:?base tree}
HEAD=${2:?head tree}
W=${3:-${TMPDIR:-/tmp}/perf-global-ab}
mkdir -p "$W/app" "$W/cg" "$W/tc-base" "$W/tc-head"
# Copies, so a build in either tree during the run doesn't change what is measured.
for side in base head; do
  tree=$BASE
  [[ $side == head ]] && tree=$HEAD
  cp "$tree/target/debug/velt" "$tree"/target/debug/libvelt_rt.a "$tree"/target/debug/libvelt_rt_shared.* "$W/tc-$side/"
done
python3 "$HEAD/bench/compile/scale/app.py" 10000 "$W/app" > /dev/null
cat > "$W/now.velt" <<'VELT'
function main() {
  const t = performance.now();
  let n = 0;
  while (performance.now() - t < 0.001) {
    n++;
  }
  console.log(n > 0);
}
VELT
export VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off
run() { # side tree row args...
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
rows=(hello now app10k check_all_std)
for row in "${rows[@]}"; do
  for side in base head; do
    tree=$BASE
    [[ $side == head ]] && tree=$HEAD
    case $row in
      hello) n=$(run $side "$tree" $row build "$tree/tests/golden/m1/hello.velt" -o "$W/hello-$side") ;;
      now) n=$(run $side "$tree" $row build "$W/now.velt" -o "$W/now-$side") ;;
      app10k) n=$(run $side "$tree" $row build "$W/app/main.velt" -o "$W/app-$side") ;;
      check_all_std) n=$(VELT_CHECK_ALL_STD=1 run $side "$tree" $row check "$tree/bench/compile/all_std.velt") ;;
    esac
    eval "${side}_$row=$n"
  done
  b=$(eval echo "\$base_$row")
  h=$(eval echo "\$head_$row")
  awk -v r="$row" -v b="$b" -v h="$h" 'BEGIN { printf "%-14s %14d %14d %+7.2f%%\n", r, b, h, (h - b) * 100 / b }'
done
