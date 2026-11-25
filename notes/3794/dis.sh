#!/bin/bash
# Disassembles one symbol (substring match) from the base and head builds of a row.
OUT=$HOME/s13; row=$1; sym=$2
for side in base head; do
  objdump -d --no-show-raw-insn "$OUT/bin/$row-$side" | awk -v s="$sym" '/^[0-9a-f]+ <.*>:$/ {p = index($0, s) > 0} p' | sed -E 's/^ *[0-9a-f]+:\t//' > "$OUT/cg/dis-$side.txt"
  echo "$side $(wc -l < $OUT/cg/dis-$side.txt) lines"
done
diff "$OUT/cg/dis-base.txt" "$OUT/cg/dis-head.txt" | head -${3:-80}
