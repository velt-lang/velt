#!/bin/bash
# The calls (bl targets) in function FN of the counted binaries $HOME/b3097fix/<name>-{base,head}
# (count.sh builds them), with how often each is written there, side by side.
#   orb -m velt-prof bash notes/3097/vm-fix/calls.sh race_select '_V4main$poll'
set -u
OUT=$HOME/b3097fix
name=$1; fn=$2
for s in base head; do
  objdump -d --no-show-raw-insn "$OUT/$name-$s" | awk -v fn="<$fn>:" '
    $2 == fn { on = 1; next }
    on && /^$/ { on = 0 }
    on && /[ \t]bl[ \t]/ { sub(/.*</, ""); sub(/>.*/, ""); sub(/\+0x[0-9a-f]+$/, ""); print }' | sort | uniq -c | sort -rn > "$OUT/calls-$s.txt"
  echo "== $s: $(awk '{n += $1} END {print n}' "$OUT/calls-$s.txt") calls"
done
diff "$OUT/calls-base.txt" "$OUT/calls-head.txt"
