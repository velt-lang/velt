#!/bin/bash
# Release build (Mac arm64, one codegen unit) of a bench row with a std variant, and the
# disassembly of sleepPoll written to dis/<row>-<variant>.txt; prints its instruction count.
#   bash dis.sh <row> <variant...>      (variant = dir under sleep-combo-var with a std/)
cd "$(dirname "$0")" || exit 1
B=/Users/andii/dev/velt/branches
V=$B/sleep-home/target/debug/velt
export SDKROOT=$(xcrun --show-sdk-path)
mkdir -p dis
row=$1; shift
for v in "$@"; do
  VELT_CODEGEN_UNITS=1 VELT_STD=$PWD/$v/std "$V" build --release "$B/sleep-home/bench/async/$row.velt" -o dis/$row-$v > dis/$row-$v.log 2>&1 || { echo "$v: build failed"; tail -3 dis/$row-$v.log; continue; }
  sym=$(nm dis/$row-$v | grep "sched__sleepN9sleepPoll$" | awk '{print $3}')
  objdump -d --no-show-raw-insn --disassemble-symbols="$sym" dis/$row-$v | grep -E "^ *[0-9a-f]+:" | sed -E 's/^ *[0-9a-f]+:\s*//' > dis/$row-$v.txt
  echo "$v: sleepPoll $(wc -l < dis/$row-$v.txt) instructions; prologue: $(head -8 dis/$row-$v.txt | grep -c -E '^(stp|sub\s+sp)')"
done
