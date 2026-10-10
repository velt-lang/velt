#!/usr/bin/env bash
# Do two compilers emit the same VIR for the programs under bench/? Prints each program whose
# VIR differs and a summary (programs either compiler cannot build are counted apart).
#
#   bench/vir_identical.sh <base velt> <head velt>
set -uo pipefail
base=$1
head=$2
cd "$(dirname "$0")"
tmp=$(mktemp -d)
same=0 differ=0 skipped=0
for f in *.vlt */*.vlt; do
  if ! "$base" build "$f" --emit vir >"$tmp/base.vir" 2>/dev/null; then
    skipped=$((skipped + 1))
    continue
  fi
  "$head" build "$f" --emit vir >"$tmp/head.vir" 2>/dev/null
  if cmp -s "$tmp/base.vir" "$tmp/head.vir"; then
    same=$((same + 1))
  else
    differ=$((differ + 1))
    echo "differs: $f"
  fi
done
rm -rf "$tmp"
echo "identical=$same differ=$differ not-built=$skipped"
