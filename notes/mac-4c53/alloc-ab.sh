#!/bin/bash
# Inside the OrbStack VM (Linux arm64): release velt + runtime for base (main 9cb569834), head
# (alloc-step-a, #2196 step A) and noattr (head with the wrappers' allocator attributes taken
# off: the break check), each in its own $HOME target; then cachegrind's instruction count of
# every bench/*.velt row and the self-contained bench/json rows, twice, built by each side with
# its own std. Ends by deleting its build directories (fstrim is run from the Mac after).
set -u
source "$HOME/.cargo/env"
B=$VELT_ROOT/branches
OUT=$HOME/alloc-out
rm -rf "$OUT"
mkdir -p "$OUT"
export CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 VELT_BUILD_CACHE=off VELT_THREADS=1
unset VELT_HEAP_HOOKS VELT_RC_STATS VELT_RT_DEBUG_ALLOC
tree_of() { case $1 in base) echo $B/alloc-base;; head) echo $B/alloc-step-a;; noattr) echo $B/alloc-noattr;; inline) echo $B/alloc-inline;; esac; }
ROWS=$(cd $B/alloc-step-a && ls bench/*.velt bench/json/parse_value.velt bench/json/parse_typed.velt bench/json/parse_union.velt bench/json/navigate.velt bench/json/edit.velt)
for side in base head noattr inline; do
  tree=$(tree_of $side)
  echo "== build $side ($(cd "$tree" && git rev-parse --short HEAD), $(cd "$tree" && git status --porcelain | wc -l) changed)"
  (cd "$tree" && CARGO_TARGET_DIR=$HOME/tgt-alloc-$side cargo build --release -p veltc --bin velt 2>&1 | tail -1) || exit 1
  (cd "$tree" && CARGO_TARGET_DIR=$HOME/tgt-alloc-$side cargo build --release -p velt_rt 2>&1 | tail -1) || exit 1
  [ -f "$HOME/tgt-alloc-$side/release/libvelt_rt.a" ] || { echo "$side: no runtime library"; exit 1; }
  mkdir -p "$OUT/$side"
  for r in $ROWS; do
    n=$(echo "$r" | tr / _ | sed 's/\.velt$//')
    VELT_STD=$tree/std VELT_RT_LIB=$HOME/tgt-alloc-$side/release/libvelt_rt.a \
      "$HOME/tgt-alloc-$side/release/velt" build "$tree/$r" --release -o "$OUT/$side/$n" > "$OUT/$side/$n.build.log" 2>&1 \
      || echo "$side: $r did not build: $(tail -2 "$OUT/$side/$n.build.log" | tr '\n' ' ' | cut -c1-200)"
  done
done
# The wrappers in head's binaries, and none in base's (a check that the sides differ as meant).
echo "head hashmap wrapper symbols: $(nm "$OUT/head/bench_hashmap" 2>/dev/null | grep -c 'velt.heap')"
echo "base hashmap wrapper symbols: $(nm "$OUT/base/bench_hashmap" 2>/dev/null | grep -c 'velt.heap')"
echo "== rows (instructions; run 1, run 2)"
for r in $ROWS; do
  n=$(echo "$r" | tr / _ | sed 's/\.velt$//')
  line="$n"
  for side in base head noattr inline; do
    runs="1"
    case $n in bench_hashmap|bench_closures) runs="1 2";; esac
    for run in $runs; do
      if [ -x "$OUT/$side/$n" ]; then
        (cd "$OUT" && valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
          "$OUT/$side/$n" > "$OUT/$side/$n.run$run.out" 2> "$OUT/$side/$n.run$run.err")
        i=$(grep -o 'I *refs: *[0-9,]*' "$OUT/$side/$n.run$run.err" | tr -dc '0-9')
      else
        i=-
      fi
      line="$line $side$run=$i"
    done
  done
  cmp -s "$OUT/base/$n.run1.out" "$OUT/head/$n.run1.out" || line="$line OUTPUT-DIFFERS"
  echo "$line"
done
echo "== cleanup"
rm -rf "$HOME/tgt-alloc-base" "$HOME/tgt-alloc-head" "$HOME/tgt-alloc-noattr" "$HOME/tgt-alloc-inline" "$OUT"
df -h / | tail -1
