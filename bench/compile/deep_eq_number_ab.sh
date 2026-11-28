#!/usr/bin/env bash
# Instructions at two toolchains for `Number(s)` of a string held as `any` and deep equality's
# cycle guard in std (velt/lang/number/parse's `toNumber`, std/lang/hooks/deep_eq.velt, #2196):
# the run of the programs that convert `any` values and compare values that can hold
# themselves, and the build cost of hello and of the 10k-line application of
# bench/compile/scale/app.py (`velt check`, a debug `velt build`, a release `velt build`) with
# binary sizes. Linux, cachegrind; build rows count the `velt` process only (the linker and clang
# are not followed), VELT_THREADS=1, a fresh VELT_HOME, the build caches off. Programs run built
# `--release`, one thread, and must print the same output on both sides.
#
#   bench/compile/deep_eq_number_ab.sh BASE_VELT BASE_STD HEAD_VELT HEAD_STD WORK [only-row]
#
# Set VELT_RT_LIB when the toolchains' target dirs have no runtime of their profile. Prints one
# line per row: name, base, head, delta. Needs: valgrind, python3.
set -euo pipefail
BV=$1 BS=$2 HV=$3 HS=$4 OUT=$5 ONLY=${6:-}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$OUT/src"
cat > "$OUT/src/hello.velt" <<'EOF'
function main() {
  console.log("hello");
}
EOF
[ -f "$OUT/src/app10k/main.velt" ] || python3 "$ROOT/bench/compile/scale/app.py" 10000 "$OUT/src/app10k" >/dev/null
export VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off
instr() { # file with cachegrind's summary
  grep -E "I +refs" "$1" | awk '{gsub(",","",$NF); print $NF}'
}
compile() { # velt std mode program tag
  local home="$OUT/home-$5"
  rm -rf "$home"
  mkdir -p "$home"
  local args=(check "$4")
  [ "$3" = debug ] && args=(build "$4" -o "$OUT/bin-$5")
  [ "$3" = release ] && args=(build --release "$4" -o "$OUT/bin-$5")
  VELT_HOME="$home" VELT_STD="$2" valgrind --tool=cachegrind --cache-sim=no \
    --cachegrind-out-file=/dev/null "$1" "${args[@]}" 2> "$OUT/cg-$5.txt" >/dev/null
  instr "$OUT/cg-$5.txt"
}
run() { # velt std program tag: builds it (release), runs it under cachegrind
  local exe="$OUT/run-$4"
  VELT_STD="$2" "$1" build --release "$3" -o "$exe" > /dev/null 2>&1
  VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
    "$exe" > "$OUT/out-$4.txt" 2> "$OUT/cg-$4.txt"
  instr "$OUT/cg-$4.txt"
}
row() { # name base head
  printf "%s\t%s\t%s\t%s\n" "$1" "$2" "$3" \
    "$(awk -v b="$2" -v h="$3" 'BEGIN{printf "%+.2f%%", (h-b)*100/b}')"
}
printf "row\tbase\thead\tdelta\n"
RUNS=(bench/stdport/parse_float.velt bench/stdport/deep_equal.velt
  bench/any_ops/hot.ts bench/any_ops/eq.ts bench/any_ops/eq_num.ts bench/any_ops/eq_null.ts
  bench/any_ops/has.ts bench/any_text/objects.ts bench/any_text/classes.ts bench/any_text/maps.ts
  bench/any_text/mixed.ts)
for p in "${RUNS[@]}"; do
  n=$(echo "$p" | tr '/' '_' | sed 's/\.velt$//; s/\.ts$//')
  [ -n "$ONLY" ] && [ "$ONLY" != "$n" ] && continue
  b=$(run "$BV" "$BS" "$ROOT/$p" "b-$n")
  h=$(run "$HV" "$HS" "$ROOT/$p" "h-$n")
  cmp -s "$OUT/out-b-$n.txt" "$OUT/out-h-$n.txt" || echo "$n: output differs" >&2
  row "run $n" "$b" "$h"
done
for n in hello app10k; do
  [ -n "$ONLY" ] && [ "$ONLY" != "$n" ] && continue
  p="$OUT/src/$n.velt"
  [ "$n" = app10k ] && p="$OUT/src/app10k/main.velt"
  for m in check debug release; do
    b=$(compile "$BV" "$BS" $m "$p" "b-$n-$m")
    h=$(compile "$HV" "$HS" $m "$p" "h-$n-$m")
    row "$m $n" "$b" "$h"
    if [ "$m" != check ]; then
      row "size $m $n" "$(stat -c %s "$OUT/bin-b-$n-$m")" "$(stat -c %s "$OUT/bin-h-$n-$m")"
    fi
  done
done
