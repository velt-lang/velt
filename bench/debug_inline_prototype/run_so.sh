#!/usr/bin/env bash
# debug builds linking LLVM-optimized std objects from a warmed cache: each program built twice,
# the second build's executable counted.
V=~/velt-build/debug-inline-m/release/velt; S=~/velt/branches/debug-inline-m/std; R=~/velt-build/debug-inline/release/libvelt_rt.a
M=~/velt-build/debug-inline/mp; O=$M/o/so; mkdir -p $O; L=${L:-so}
export VELT_RT_LIB=$R VELT_STD=$S VELT_HOME=$M/home-$L VELT_DI_STDOBJ=1 VELT_DI_SMALL=0 VELT_STD_OBJECT_MIN_MISSES=0
for f in ${PROGS:-$M/p/*.velt}; do n=$(basename $f .velt); [ $n = hello ] && continue
  $V build $f -o $O/$n.cold > $O/$n.bl 2>&1 || { echo "$n BUILD-FAIL"; head -3 $O/$n.bl; continue; }
  $V build $f -o $O/$n >> $O/$n.bl 2>&1
  c=$(VELT_THREADS=1 VELT_RESCUE=0 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null $O/$n 2>&1 >/dev/null </dev/null | awk '/I +refs/{gsub(",","",$NF);print $NF}')
  # same output as the plain debug build?
  VELT_THREADS=1 $O/$n > $O/$n.out 2>&1; cmp -s $O/$n.out <(VELT_THREADS=1 $M/o/l0/$n 2>&1) || echo "$n OUTPUT-DIFFERS" >&2
  echo -e "$n\t$c"
done | tee $M/o/$L.tsv
