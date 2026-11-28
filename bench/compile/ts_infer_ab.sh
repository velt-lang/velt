#!/usr/bin/env bash
# Instructions of `velt build` (debug) at two toolchains for #3116, #3091 and #3087: hello, the
# 10k-line application of bench/compile/scale/app.py, and the area's goldens (an object literal
# leaving out a type parameter's fields, a record view spread and matched as its record, `in` on a
# view, also with record accessors, and that one with an `in` on the `any` too). Then the instructions of running the `in` golden's program.
# Linux, cachegrind, the `velt` process only (the linker is not counted), VELT_THREADS=1, a fresh
# VELT_HOME, the build caches off.
#
#   bench/compile/ts_infer_ab.sh BASE_VELT BASE_STD HEAD_VELT HEAD_STD WORK
#
# Prints one line per program: name, what, base, head, delta. Needs: valgrind, python3.
set -euo pipefail
BV=$1 BS=$2 HV=$3 HS=$4 OUT=$5
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$OUT/src"
cat > "$OUT/src/hello.velt" <<'VELT'
function main() {
  console.log("hello");
}
VELT
# The `in` golden with one `in` on the `any` itself: a view's `in` is one on its source, so this
# row is what is left once the program pays for `in` on an `any` either way.
sed 's/^  const r = a as Record<string, any>;$/&\n  console.log("label" in a);/' \
  "$ROOT/tests/golden/lang/record_view_in_inherited.velt" > "$OUT/src/in_any_too.velt"
[ -f "$OUT/src/app10k/main.velt" ] || python3 "$ROOT/bench/compile/scale/app.py" 10000 "$OUT/src/app10k" >/dev/null
G="$ROOT/tests/golden/lang"
export VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off
refs() { grep -E "I +refs" "$1" | awk '{gsub(",","",$NF); print $NF}'; }
build() { # velt std program tag
  local home="$OUT/home-$4"; mkdir -p "$home"
  VELT_HOME="$home" VELT_STD="$2" valgrind --tool=cachegrind --cache-sim=no \
    --cachegrind-out-file=/dev/null --error-exitcode=0 "$1" build "$3" -o "$OUT/bin-$4" \
    2> "$OUT/cg-$4.txt" >/dev/null || return 1
  refs "$OUT/cg-$4.txt"
}
run() { # tag
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$OUT/bin-$1" \
    2> "$OUT/cg-run-$1.txt" >/dev/null
  refs "$OUT/cg-run-$1.txt"
}
row() { printf "%s\t%s\t%s\t%s\t%s\n" "$1" "$2" "$3" "$4" \
  "$(awk -v b="$3" -v h="$4" 'BEGIN{printf "%+.2f%%", (h-b)*100/b}')"; }
printf "program\twhat\tbase\thead\tdelta\n"
for n in hello app10k infer_left_out_field record_view_spread_wider record_view_in_inherited record_view_in_keyed in_any_too; do
  case $n in
    hello | in_any_too) p="$OUT/src/$n.velt" ;;
    app10k) p="$OUT/src/app10k/main.velt" ;;
    *) p="$G/$n.velt" ;;
  esac
  b=$(build "$BV" "$BS" "$p" "b-$n") || b=fail
  [ -s "$OUT/bin-b-$n" ] || b=fail
  h=$(build "$HV" "$HS" "$p" "h-$n")
  if [ "$b" = fail ]; then
    printf "%s\tbuild\t(fails at base)\t%s\t\n" "$n" "$h"
  else
    row "$n" build "$b" "$h"
  fi
done
# `in` on a view at run time: base answers differently (the bug), so this is head's cost only
# next to hello's run.
row hello run "$(run b-hello)" "$(run h-hello)"
printf "record_view_in_inherited\trun\t-\t%s\n" "$(run h-record_view_in_inherited)"
