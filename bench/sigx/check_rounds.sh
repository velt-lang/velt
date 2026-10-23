#!/usr/bin/env bash
# usage: bench/sigx/check_rounds.sh <velt> <sigx checkout> <file list> <outdir>
# Runs `velt check` on each entry file of the list (paths relative to the checkout, 8 in
# parallel) with VELT_SEMA_TIMINGS=1, one log per file in <outdir>, and prints the wall time,
# how many checks of the program sema made in all (its rounds), and how many reports say the
# inference of function types written without `throws` hit its limit.
set -u
velt="$1"; root="$2"; list="$3"; out="$4"
mkdir -p "$out"
export velt out
cd "$root"
one() {
  n=$(echo "$1" | tr "/" "_")
  VELT_SEMA_TIMINGS=1 "$velt" check "$1" > "$out/$n.log" 2>&1
  echo "exit=$?" >> "$out/$n.log"
}
export -f one
start=$(date +%s%N)
xargs -P 8 -I{} bash -c 'one "$@"' _ {} < "$list"
end=$(date +%s%N)
echo "wall ms $(( (end - start) / 1000000 ))"
echo "checks $(cat "$out"/*.log | grep '^velt-sema:   round' | grep -vc '  x[0-9]')"
echo "limit notes $(cat "$out"/*.log | grep -c 'hit its limit')"
