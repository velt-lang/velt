#!/bin/bash
# Exports std of git ref $1 into $HOME/b3097fix/std-<ref> (in the VM): another head to count.
#   orb -m velt-prof bash notes/3097/vm-fix/export.sh 5cd66ec71
set -eu
ref=$1
OUT=$HOME/b3097fix/std-$ref
rm -rf "$OUT" "$OUT.tmp" && mkdir -p "$OUT.tmp"
cd /Users/andii/dev/velt/branches/cross-wake
git archive "$ref" std | tar -x -C "$OUT.tmp"
mv "$OUT.tmp/std" "$OUT" && rmdir "$OUT.tmp"
echo "$OUT"
