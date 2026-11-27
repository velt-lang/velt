#!/usr/bin/env bash
# Instructions of `velt check` and `velt build` (debug) at two toolchains, for what loads
# velt/sync/epoch (lazy std's `Gate::RefAtomic`): hello, hello with an `Atomic<u64>`, an
# `Atomic<shared<B>>`, a member of an `any` read (every live class's methods live), and the
# 10k-line application of bench/compile/scale/app.py. Linux, cachegrind, the `velt` process only
# (the linker is not counted), VELT_THREADS=1, a fresh VELT_HOME, the build caches off.
#
#   bench/compile/atomic_epoch_ab.sh BASE_VELT BASE_STD HEAD_VELT HEAD_STD WORK
#
# Set VELT_RT_LIB when the toolchains' target dirs have no runtime of their profile. Prints one
# line per program and mode: name, mode, base, head, delta. Needs: valgrind, python3.
set -euo pipefail
BV=$1 BS=$2 HV=$3 HS=$4 OUT=$5
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$OUT/src"
cat > "$OUT/src/hello.velt" <<'EOF'
function main() {
  console.log("hello");
}
EOF
cat > "$OUT/src/scalar.velt" <<'EOF'
function main() {
  const a = new Atomic<u64>(1);
  a.store(2);
  console.log("hello");
}
EOF
cat > "$OUT/src/shared.velt" <<'EOF'
interface B {
  n: u64;
}
function main() {
  const a = new Atomic<shared<B>>(shared({ n: 1 }));
  a.store(shared({ n: 2 }));
  console.log("hello", a.load().n);
}
EOF
cat > "$OUT/src/anymember.velt" <<'EOF'
function f(x: any) {
  console.log(x.a);
}
function main() {
  f({ a: 1 });
}
EOF
[ -f "$OUT/src/app10k/main.velt" ] || python3 "$ROOT/bench/compile/scale/app.py" 10000 "$OUT/src/app10k" >/dev/null
export VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off
count() { # velt std mode program tag
  local home="$OUT/home-$5"; mkdir -p "$home"
  local args=(check "$4"); [ "$3" = build ] && args=(build "$4" -o "$OUT/bin-$5")
  VELT_HOME="$home" VELT_STD="$2" valgrind --tool=cachegrind --cache-sim=no \
    --cachegrind-out-file=/dev/null "$1" "${args[@]}" 2> "$OUT/cg-$5.txt" >/dev/null
  grep -E "I +refs" "$OUT/cg-$5.txt" | awk '{gsub(",","",$NF); print $NF}'
}
printf "program\tmode\tbase\thead\tdelta\n"
for n in hello scalar shared anymember app10k; do
  p="$OUT/src/$n.velt"; [ "$n" = app10k ] && p="$OUT/src/app10k/main.velt"
  for m in check build; do
    b=$(count "$BV" "$BS" $m "$p" "b-$n-$m")
    h=$(count "$HV" "$HS" $m "$p" "h-$n-$m")
    printf "%s\t%s\t%s\t%s\t%s\n" "$n" $m "$b" "$h" \
      "$(awk -v b="$b" -v h="$h" 'BEGIN{printf "%+.2f%%", (h-b)*100/b}')"
  done
done
