#!/usr/bin/env bash
# Per-function cachegrind of b vs n (ab4.sh's trees, same compiler) on the rows given, built with
# -g; prints the functions whose instruction counts differ by >= 200k.
set -uo pipefail
W=$HOME/ab-3289
mkdir -p "$W/prof"
cd "$W/prof"
for r in "$@"; do
  for v in b n; do
    exe=$W/prof/$r-$v
    VELT_STD=$W/$v-src/std VELT_RT_LIB=$W/tc/libvelt_rt.a "$W/tc/velt" build --release -g --backend llvm "$W/$v-src/bench/async/$r.velt" -o "$exe" > "$exe.build" 2>&1 || { echo "build FAIL $r-$v"; continue; }
    VELT_THREADS=1 valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file="$exe.cg" "$exe" < /dev/null > /dev/null 2>&1
    cg_annotate --auto=no "$exe.cg" 2>/dev/null | sed -n '/Function:file summary/,$p' | grep -E '^[<>] ' |
      awk '{gsub(",","",$2); n=$2; f=$NF; sub(":$","",f); sub(/:\/.*/,"",f); a[f]+=n} END {for (f in a) print f, a[f]}' | sort > "fn-$r-$v.txt"
  done
  echo "### $r"
  join -a1 -a2 -e0 -o 0,1.2,2.2 "fn-$r-b.txt" "fn-$r-n.txt" |
    awk '{d=$3-$2; a=d<0?-d:d; if (a>=200000) printf "%14d %14d %+12d  %s\n", $2, $3, d, $1}' | sort -k3 -n
done
echo "== prof end"
