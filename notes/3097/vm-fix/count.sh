#!/bin/bash
# Instructions (cachegrind, all threads) of bench/async programs built with base std and with the
# cross-wake worktree's std (head), one compiler ($HOME/b3097fix, build.sh), as bench/nightly.sh
# counts them (VELT_RESCUE=0). FUNCS=1 also writes cg_annotate per-function tables
# ($HOME/b3097fix/<name>-T<t>-<side>.txt) to compare.
#   orb -m velt-prof bash notes/3097/vm-fix/count.sh "race_select timers_fire_phases" "1 4" [runs]
set -u
OUT=$HOME/b3097fix
WT=/Users/andii/dev/velt/branches/cross-wake
V=${VELT:-$OUT/tgt/release/velt}
RT=$OUT/tgt/release/libvelt_rt.a
names=$1; threads=${2:-1}; runs=${3:-1}
printf "%-28s %3s %14s %14s %8s\n" bench T base head delta
for n in $names; do
  src=$WT/bench/async/$n.velt
  [ -f "$src" ] || src=$WT/bench/async/scheduler/$n.velt
  for s in base head; do
    std=${BASE_STD:-$OUT/base-std}; [ $s = head ] && std=${HEAD_STD:-$WT/std}
    g=""; [ "${FUNCS:-0}" = 1 ] && g="-g"
    VELT_STD=$std VELT_RT_LIB=$RT "$V" build --release $g --backend llvm "$src" -o "$OUT/$n-$s" \
      >"$OUT/$n-$s.build" 2>&1 || { echo "$n $s: build failed"; tail -3 "$OUT/$n-$s.build"; continue 2; }
  done
  for t in $threads; do
    declare -A best=()
    for r in $(seq 1 "$runs"); do
      for s in base head; do
        f=/dev/null; [ "${FUNCS:-0}" = 1 ] && f=$OUT/$n-T$t-$s.cg
        refs=$(VELT_THREADS=$t VELT_RESCUE=0 valgrind --tool=cachegrind --cache-sim=no \
          --cachegrind-out-file="$f" "$OUT/$n-$s" 2>&1 >/dev/null | awk '/I *refs:/ {gsub(",", "", $NF); print $NF}')
        if [ -z "${best[$s]:-}" ] || [ "$refs" -lt "${best[$s]}" ]; then best[$s]=$refs; fi
        if [ "${FUNCS:-0}" = 1 ]; then
          cg_annotate "$f" 2>/dev/null | sed -n '1,80p' >"$OUT/$n-T$t-$s.txt"
        fi
      done
    done
    b=${best[base]}; h=${best[head]}
    awk -v n="$n" -v t="$t" -v b="$b" -v h="$h" 'BEGIN { printf "%-28s %3s %14s %14s %+7.2f%%\n", n, t, b, h, (h - b) * 100 / b }'
    unset best
  done
done
