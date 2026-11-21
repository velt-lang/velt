#!/bin/bash
# In the VM: instructions per source FILE and, for sched_sleep.velt, per source LINE, base and
# head, from the callgrind profiles (built with -g).   orb -m velt-prof bash vm-lines.sh <row>
OUT=$HOME/r3273/cg
row=$1
for side in base head; do
  callgrind_annotate --threshold=100 "$OUT/$row-$side.out" 2>/dev/null | grep -E "^ *[0-9,]+ \(" |
    sed -E 's/ \[[^]]*\]$//' | awk '{n=$1; gsub(",", "", n); f=$NF; sub(/:.*/, "", f); sub(/.*\//, "", f); t[f]+=n} END{for (f in t) print t[f], f}' | sort -k2 > "$OUT/$row-$side.files"
done
echo "== per file (difference over 20k)"
join -1 2 -2 2 -a 1 -a 2 -e 0 -o 0,1.1,2.1 "$OUT/$row-base.files" "$OUT/$row-head.files" | awk '{d=$3-$2; if (d>20000 || d<-20000) printf "%+9d  %9d -> %9d  %s\n", d, $2, $3, $1}' | sort -rn | head -20
std=/Users/andii/dev/velt/branches
for side in base head; do
  src=$std/main-base/std/lang/hooks/sched_sleep.velt; [ $side = head ] && src=$std/sleep-home/std/lang/hooks/sched_sleep.velt
  echo "== sched_sleep.velt lines, $side (over 150k)"
  callgrind_annotate --auto=yes --show-percs=no "$OUT/$row-$side.out" "$src" 2>/dev/null | awk -v s="$src" '$0 ~ "-- Auto-annotated source: " s {f=1; next} /^-- Auto-annotated source/ {f=0} f' | grep -E "^ *[0-9,]+ " | awk '{n=$1; gsub(",", "", n); if (n+0 >= 150000) print}' | cut -c1-130 | head -40
done
