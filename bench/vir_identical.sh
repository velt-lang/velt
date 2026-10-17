#!/usr/bin/env bash
# usage: bench/vir_identical.sh <base velt> <branch velt> <src dir> <out dir>
# Emits VIR for every .vlt under <src dir>/bench with both compilers, each run from the same
# place (<out dir>/vt/bin with the source tree's std next to it, so embedded paths agree), and
# counts identical files.
set -u
base="$1"; br="$2"; src="$3"; out="$4"
rm -rf "$out/vt"; mkdir -p "$out/base" "$out/br" "$out/vt/bin"
cp -r "$src/std" "$out/vt/std"
files=$(cd "$src" && find bench -name "*.vlt" | sort)
for side in base br; do
  if [ $side = base ]; then cp "$base" "$out/vt/bin/velt.exe"; else cp "$br" "$out/vt/bin/velt.exe"; fi
  for f in $files; do
    n=$(echo "$f" | tr '/' '_')
    (cd "$src" && "$out/vt/bin/velt.exe" build "$f" --emit vir > "$out/$side/$n.vir" 2>/dev/null; echo $? > "$out/$side/$n.status")
  done
done
same=0; differ=0; fail=0; difflist=""
for f in $files; do
  n=$(echo "$f" | tr '/' '_')
  a=$(cat "$out/base/$n.status"); b=$(cat "$out/br/$n.status")
  if [ "$a" != 0 ] || [ "$b" != 0 ]; then
    if [ "$a" != "$b" ]; then fail=$((fail+1)); difflist="$difflist FAIL:$f($a/$b)"; fi
    continue
  fi
  if cmp -s "$out/base/$n.vir" "$out/br/$n.vir"; then same=$((same+1)); else differ=$((differ+1)); difflist="$difflist $f"; fi
done
echo "identical=$same differ=$differ build-status-differs=$fail"
echo "$difflist" | tr ' ' '\n' | sed '/^$/d'
