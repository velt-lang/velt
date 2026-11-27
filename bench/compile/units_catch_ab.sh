#!/usr/bin/env bash
# Instructions of `velt check` and `velt build` (debug) at two toolchains, whole and by units
# (VELT_UNITS=2), for a check by units demanding std's panic path (#3948): hello, hello under
# `catchPanic`, and the 10k-line application of bench/compile/scale/app.py. Linux, cachegrind,
# the `velt` process only (the linker is not counted), VELT_THREADS=1, a fresh VELT_HOME, the
# build caches off.
#
#   bench/compile/units_catch_ab.sh BASE_VELT BASE_STD HEAD_VELT HEAD_STD WORK
#
# Set VELT_RT_LIB when the toolchains' target dirs have no runtime of their profile. Prints one
# line per program, mode and units: name, mode, units, base, head, delta. Needs: valgrind,
# python3.
set -euo pipefail
BV=$1 BS=$2 HV=$3 HS=$4 OUT=$5
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$OUT/src"
cat > "$OUT/src/hello.velt" <<'EOF'
function main() {
  console.log("hello");
}
EOF
cat > "$OUT/src/caught.velt" <<'EOF'
import { catchPanic } from "velt/task";

async function boom(): Promise<number> {
  await yieldNow();
  panic("bomb");
  return 1;
}

async function main() {
  try {
    await catchPanic(() => boom());
  } catch (e) {
    console.log("caught");
  }
}
EOF
[ -f "$OUT/src/app10k/main.velt" ] || python3 "$ROOT/bench/compile/scale/app.py" 10000 "$OUT/src/app10k" >/dev/null
export VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off
count() { # velt std mode program units tag
  local home="$OUT/home-$6"; mkdir -p "$home"
  local args=(check "$4"); [ "$3" = build ] && args=(build "$4" -o "$OUT/bin-$6")
  local env=(env -u VELT_UNITS); [ "$5" = 2 ] && env=(env VELT_UNITS=2)
  "${env[@]}" VELT_HOME="$home" VELT_STD="$2" valgrind --tool=cachegrind --cache-sim=no \
    --cachegrind-out-file=/dev/null "$1" "${args[@]}" 2> "$OUT/cg-$6.txt" >/dev/null
  grep -E "I +refs" "$OUT/cg-$6.txt" | awk '{gsub(",","",$NF); print $NF}'
}
printf "program\tmode\tunits\tbase\thead\tdelta\n"
for n in hello caught app10k; do
  p="$OUT/src/$n.velt"; [ "$n" = app10k ] && p="$OUT/src/app10k/main.velt"
  for u in 0 2; do
    for m in check build; do
      b=$(count "$BV" "$BS" $m "$p" $u "b-$n-$m-$u")
      h=$(count "$HV" "$HS" $m "$p" $u "h-$n-$m-$u")
      printf "%s\t%s\t%s\t%s\t%s\t%s\n" "$n" $m $u "$b" "$h" \
        "$(awk -v b="$b" -v h="$h" 'BEGIN{printf "%+.2f%%", (h-b)*100/b}')"
    done
  done
done
