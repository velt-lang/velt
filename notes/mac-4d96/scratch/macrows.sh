#!/bin/bash
# Rough Mac signal (instructions retired, T1, best of 3) for std variants; the VM's cachegrind decides.
#   bash macrows.sh <row> <variant...>
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/4d96d609-2239-43e8-bcbb-b3568d1a3979/scratchpad
B=/Users/andii/dev/velt/branches
V=$B/sleep-home/target/debug/velt
row=$1; shift
base=
for v in "$@"; do
  std=$B/sleep-combo-var/$v/std
  [ "$v" = base ] && std=$B/main-base/std
  VELT_CODEGEN_UNITS=1 VELT_STD=$std "$V" build --release "$B/sleep-combo/bench/async/$row.velt" -o "$S/$row-$v" > "$S/$row-$v.log" 2>&1 || { echo "build $v failed"; tail -3 "$S/$row-$v.log"; continue; }
  best=
  for i in 1 2 3; do
    n=$( { VELT_THREADS=1 /usr/bin/time -l "$S/$row-$v" > /dev/null; } 2>&1 | awk '/instructions retired/ {print $1}')
    if [ -z "$best" ] || [ "$n" -lt "$best" ]; then best=$n; fi
  done
  [ -z "$base" ] && base=$best
  echo "$row $v: $best  $(( (best - base) * 10000 / base ))bp vs $1"
done
