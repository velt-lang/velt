#!/usr/bin/env bash
# usage: bench/sigx/check_cpu.sh <base velt> <base std> <head velt> <head std> <sigx checkout>
#          <file list> <rounds>
# `velt check` user CPU on each entry file of the list (paths relative to the checkout), the
# base and the head compiler interleaved file by file, each with its own std ($VELT_STD).
# Prints, for each compiler, the sum over the files of the smallest user time of the rounds,
# the difference, and the load average.
set -u
base="$1"; bstd="$2"; head="$3"; hstd="$4"; root="$5"; list="$6"; rounds="$7"
cd "$root"
TIMEFORMAT=%U
user() {
  { time VELT_STD="$2" "$1" check "$3" < /dev/null > /dev/null 2>&1; } 2>&1
}
tb=0; th=0
while read -r f; do
  mb=999999; mh=999999
  for _ in $(seq "$rounds"); do
    mb=$(awk -v a="$(user "$base" "$bstd" "$f")" -v b="$mb" 'BEGIN { print (a < b) ? a : b }')
    mh=$(awk -v a="$(user "$head" "$hstd" "$f")" -v b="$mh" 'BEGIN { print (a < b) ? a : b }')
  done
  tb=$(awk -v a="$tb" -v b="$mb" 'BEGIN { print a + b }')
  th=$(awk -v a="$th" -v b="$mh" 'BEGIN { print a + b }')
done < "$list"
awk -v b="$tb" -v h="$th" 'BEGIN { printf "base %.2f s user, head %.2f s user, %+.2f %%\n", b, h, (h - b) / b * 100 }'
uptime
