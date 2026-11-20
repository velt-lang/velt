#!/usr/bin/env bash
# After ab7.sh (its executables in ~/ab-3289/cg): min of N cachegrind runs at VELT_THREADS=4 for
# the rows given, trees b, p and m. Usage: t4min.sh N row...
W=$HOME/ab-3289; N=$1; shift
one() { (cd "$W/cg" && VELT_THREADS=4 timeout 1800 valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$W/cg/$1-$2" < /dev/null 2>&1 > /dev/null | grep -oP 'I\s+refs:\s+\K[0-9,]+' | tr -d ,); }
for r in "$@"; do
  line="$r t4 min of $N:"
  declare -A mn
  for v in b p m; do
    best=
    for i in $(seq "$N"); do x=$(one "$r" "$v"); [[ -z $best || $x -lt $best ]] && best=$x; done
    mn[$v]=$best; line="$line $v=$best"
  done
  python3 -c "import sys; b,p,m=map(int,sys.argv[2:5]); print(sys.argv[1], f'| b>p {(p-b)/b*100:+.2f}% | m>p {(p-m)/m*100:+.2f}%')" "$line" "${mn[b]}" "${mn[p]}" "${mn[m]}"
done
echo "== t4min end"
