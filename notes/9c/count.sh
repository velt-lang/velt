#!/bin/bash
# Instructions (cachegrind, all threads, min of RUNS) of programs built with base std and with the
# head worktree's std, one compiler ($HOME/b9c, build.sh). Programs: bench/async names, or paths.
#   orb -m velt-prof bash notes/9c/count.sh "race_select /path/x.velt" "1 4" [runs]
# HEAD_WT (default module-lock-velt) gives the head std and the bench sources.
set -u
OUT=${B9C_OUT:-$HOME/b9c}
WT=${HEAD_WT:-/Users/andii/dev/velt/branches/module-lock-velt}
V=${VELT:-$OUT/tgt/release/velt}
RT=$OUT/tgt/release/libvelt_rt.a
names=$1; threads=${2:-1}; runs=${3:-1}
printf "%-28s %3s %14s %14s %8s\n" bench T base head delta
for n in $names; do
  src=$n
  [ -f "$src" ] || src=$WT/bench/async/$n.velt
  [ -f "$src" ] || src=$WT/bench/async/scheduler/$n.velt
  b=$(basename "$src" .velt)
  for s in base head; do
    std=${BASE_STD:-$OUT/base-std}; [ $s = head ] && std=${HEAD_STD:-$WT/std}
    rm -f "$OUT/$b-$s"
    VELT_BUILD_CACHE=off VELT_STD=$std VELT_RT_LIB=$RT "$V" build --release --backend llvm "$src" \
      -o "$OUT/$b-$s" >"$OUT/$b-$s.build" 2>&1 || { echo "$b $s: build failed"; tail -3 "$OUT/$b-$s.build"; continue 2; }
  done
  for t in $threads; do
    declare -A best=()
    for r in $(seq 1 "$runs"); do
      for s in base head; do
        refs=$(VELT_THREADS=$t VELT_RESCUE=0 valgrind --tool=cachegrind --cache-sim=no \
          --cachegrind-out-file=/dev/null "$OUT/$b-$s" 2>&1 >/dev/null | awk '/I *refs:/ {gsub(",", "", $NF); print $NF}')
        if [ -z "${best[$s]:-}" ] || [ "$refs" -lt "${best[$s]}" ]; then best[$s]=$refs; fi
      done
    done
    awk -v n="$b" -v t="$t" -v b="${best[base]}" -v h="${best[head]}" \
      'BEGIN { printf "%-28s %3s %14s %14s %+7.2f%%\n", n, t, b, h, (h - b) * 100 / b }'
    unset best
  done
done
