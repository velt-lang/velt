#!/usr/bin/env bash
# Instructions of `velt check` and `velt build` (debug) at two toolchains for the casts and
# conversions of #2787, #2788, #3073 and #3115: hello, the 10k-line application of
# bench/compile/scale/app.py, and the goldens of the area (interface casts to unions of
# extensions, bound conversions, object copies with optional fields), plus the run of the
# built area programs. Linux, cachegrind, the `velt` process only (the linker is not
# counted), VELT_THREADS=1, a fresh VELT_HOME, the build caches off.
#
#   bench/compile/sigx_casts_ab.sh BASE_VELT BASE_STD HEAD_VELT HEAD_STD WORK
#
# Prints one line per program and mode: name, mode, base, head, delta. Needs: valgrind,
# python3.
set -euo pipefail
BV=$1 BS=$2 HV=$3 HS=$4 OUT=$5
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
G=$ROOT/tests/golden/lang
mkdir -p "$OUT/src"
cat > "$OUT/src/hello.velt" <<'EOF2'
function main() {
  console.log("hello");
}
EOF2
[ -f "$OUT/src/app10k/main.velt" ] || python3 "$ROOT/bench/compile/scale/app.py" 10000 "$OUT/src/app10k" >/dev/null
export VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off
count() { # velt std mode program tag
  local home="$OUT/home-$5"; mkdir -p "$home"
  local args=(check "$4"); [ "$3" = build ] && args=(build "$4" -o "$OUT/bin-$5")
  if [ "$3" = run ]; then
    valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
      "$OUT/bin-${5/run/build}" 2> "$OUT/cg-$5.txt" >/dev/null || true
  else
    VELT_HOME="$home" VELT_STD="$2" valgrind --tool=cachegrind --cache-sim=no \
      --cachegrind-out-file=/dev/null "$1" "${args[@]}" 2> "$OUT/cg-$5.txt" >/dev/null || true
  fi
  grep -E "I +refs" "$OUT/cg-$5.txt" | awk '{gsub(",","",$NF); print $NF}'
}
printf "program\tmode\tbase\thead\tdelta\n"
progs=(hello app10k iface_downcast iface_downcast_class_shaped bound_result_widen
  object_copy_optional_unknown iface_downcast_union)
for n in "${progs[@]}"; do
  case $n in
    hello) p="$OUT/src/hello.velt" ;;
    app10k) p="$OUT/src/app10k/main.velt" ;;
    *) p="$G/$n.velt" ;;
  esac
  modes=(check build)
  case $n in hello|app10k) ;; *) modes+=(run) ;; esac
  for m in "${modes[@]}"; do
    # The area's new programs don't compile at the base: their rows are head only.
    b=$(count "$BV" "$BS" $m "$p" "b-$n-$m"); b=${b:-0}
    h=$(count "$HV" "$HS" $m "$p" "h-$n-$m")
    printf "%s\t%s\t%s\t%s\t%s\n" "$n" $m "$b" "$h" \
      "$(awk -v b="$b" -v h="$h" 'BEGIN{ if (b == 0) print "n/a"; else printf "%+.2f%%", (h-b)*100/b }')"
  done
done
