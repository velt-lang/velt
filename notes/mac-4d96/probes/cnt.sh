#!/bin/bash
# The handed-on probes with counting copies of the std before the cuts (sleep-home) and with them
# (sleep-combo): outcomes, and the sums of what release found requested sleeps as.
#   bash cnt.sh <runs> <probe>...
cd "$(dirname "$0")" || exit 1
runs=$1; shift
B=/Users/andii/dev/velt/branches
python3 mkcnt.py $B/sleep-home/std "$PWD/std-cnt-before" > /dev/null || exit 1
python3 mkcnt.py $B/sleep-combo/std "$PWD/std-cnt-after" > /dev/null || exit 1
for label in before after; do
  for n in "$@"; do : > "$n.cnt-$label.counts"; done
  bash stress.sh cnt-$label "$PWD/std-cnt-$label" "$runs" "$@" | cut -c1-110
  for n in "$@"; do
    echo "sums $label $n: $(awk '{for (i = 1; i < NF; i += 2) t[$i] += $(i + 1)} END {for (k in t) printf "%s%d ", k, t[k]}' "$n.cnt-$label.counts")"
  done
done
