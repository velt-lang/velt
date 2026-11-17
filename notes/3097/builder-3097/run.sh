#!/usr/bin/env bash
# #3097 final table on a quiet Linux machine. Usage:
#   run.sh <base checkout> <step-2 checkout> <head checkout>
# base = origin/main the stack is on, s2 = branch timer-unanchor (step 2 alone), head = branch
# cross-wake (steps 1+2 with gather-back). Each side is built with its own compiler and runtime
# (release) into its own target dir under $OUT. Rows, every one interleaved base/s2/head:
#  1. every bench/async program: instructions (valgrind cachegrind, all threads), T=1 and T=4,
#     best of 5 (base and head only);
#  2. xwake.velt (next to this script): the c=1 round trip between two pinned tasks, 5 runs;
#  3. idle.velt: CPU of an idle program with a 10 ms ticker over 5 s, 5 runs;
#  4. examples/apps/actors `calls --scenario warm|fan --c 1|64|512 --n 400000 --shards 4`:
#     wall ms and cores busy ((user+sys)/real), best of 5;
#  5. `bench --duration 1000 --shards 4` (all scenarios), 5 runs.
# Output: one line per measurement on stdout; analyze with analyze.py.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
declare -A WT=([base]=$1 [s2]=$2 [head]=$3)
OUT=${OUT:-$HOME/b3097}; mkdir -p "$OUT"
TIMEFORMAT='%R %U %S'
N=${N:-5}
for s in base s2 head; do
  (cd "${WT[$s]}" && CARGO_TARGET_DIR=$OUT/tgt-$s CARGO_INCREMENTAL=0 cargo build --release -p veltc --bin velt 2>&1 | tail -1)
  (cd "${WT[$s]}" && CARGO_TARGET_DIR=$OUT/tgt-$s CARGO_INCREMENTAL=0 cargo build --release -p velt_rt 2>&1 | tail -1)
done
velt_of() { echo "$OUT/tgt-$1/release/velt"; }
build() {
  local s=$1 src=$2 exe=$3
  VELT_STD=${WT[$s]}/std VELT_RT_LIB=$OUT/tgt-$s/release/libvelt_rt.a "$(velt_of $s)" build --release "$src" -o "$exe" >"$exe.build" 2>&1 || echo "build $exe failed"
}
progs=$(cd "${WT[head]}/bench/async" && ls *.velt scheduler/*.velt 2>/dev/null)
for s in base head; do
  for p in $progs; do
    build $s "${WT[$s]}/bench/async/$p" "$OUT/$(echo ${p%.velt} | tr / _)-$s"
  done
done
for s in base s2 head; do
  build $s "$HERE/xwake.velt" "$OUT/xwake-$s"
  build $s "$HERE/idle.velt" "$OUT/idle-$s"
  (cd "${WT[$s]}/examples/apps/actors" && VELT_STD=${WT[$s]}/std VELT_RT_LIB=$OUT/tgt-$s/release/libvelt_rt.a "$(velt_of $s)" build --release -o "$OUT/actors-$s" >"$OUT/actors-$s.build" 2>&1) || echo "build actors-$s failed"
done
irefs() { local e=$1; shift; env -u VELT_RESCUE $e valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null | awk '/I *refs:/ {gsub(",","",$NF); print $NF}'; }
for round in $(seq $N); do
  for c in "T1:VELT_THREADS=1" "T4:VELT_THREADS=4"; do
    for p in $progs; do
      n=$(echo ${p%.velt} | tr / _)
      for s in base head; do
        [ -x "$OUT/$n-$s" ] && echo "IR $round ${c%%:*} $n $s $(irefs "${c#*:}" "$OUT/$n-$s")"
      done
    done
  done
done
for round in $(seq $N); do
  for s in base s2 head; do
    VELT_THREADS=4 "$OUT/xwake-$s" | sed "s/^/XWAKE $round $s /"
    r=$( { time env VELT_THREADS=4 "$OUT/idle-$s" >/dev/null 2>&1; } 2>&1 | tail -1)
    echo "IDLE $round $s | $r"
  done
done
for round in $(seq $N); do
  for sc in warm fan; do
    for cc in 1 64 512; do
      for s in base s2 head; do
        r=$( { time "$OUT/actors-$s" calls --scenario $sc --c $cc --n 400000 --shards 4 >"$OUT/calls.out" 2>/dev/null; } 2>&1 | tail -1)
        echo "CALLS $round $sc c=$cc $s $(grep -o '[0-9]* ms' "$OUT/calls.out" | tail -1) | $r"
      done
    done
  done
done
for round in $(seq $N); do
  for s in base s2 head; do
    echo "== BENCH $round $s"
    "$OUT/actors-$s" bench --duration 1000 --shards 4 2>&1 | grep -E '▸|ops_per_sec|p50' | grep -v 'mem/'
  done
done
