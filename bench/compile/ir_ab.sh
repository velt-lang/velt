#!/usr/bin/env bash
# Whether two compilers give the same release LLVM IR for the programs bench/nightly.sh builds, and
# for each that differs, the instructions the built program runs, by cachegrind (Linux).
#
#   bench/compile/ir_ab.sh TOOLCHAIN_A TOOLCHAIN_B WORK
#
# A toolchain is a directory holding `velt`, `libvelt_rt.a` and `std`. The programs come from this
# checkout, so paths in the IR match. Prints `<program> same`, or `<program> diff <a-Ir> <b-Ir>
# <change> <same|DIFF output>`, then the number of each. JOBS: programs at a time (default 4).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
A=$(realpath "$1"); B=$(realpath "$2"); mkdir -p "$3"; W=$(realpath "$3")
export A B W ROOT VELT_BUILD_CACHE=off
export STDPORT_INPUTS=${STDPORT_INPUTS:-$W/stdport-inputs}

irs() { VELT_STD=$1/std VELT_RT_LIB=$1/libvelt_rt.a "$1/velt" build --release --emit llvm "$2"; }
ir_count() {
  local exe=$1
  VELT_THREADS=1 VELT_RESCUE=0 timeout "${TO:-900}" valgrind --tool=cachegrind --cache-sim=no \
    --cachegrind-out-file=/dev/null "$exe" </dev/null >"$exe.out" 2>"$exe.vg" || true
  sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' "$exe.vg" | tr -d ,
}
one() {
  local src=$1 rel name a b
  rel=${src#"$ROOT"/bench/}
  name=$(echo "$rel" | sed 's#/#__#g; s#\.velt$##')
  a=$W/$name.a; b=$W/$name.b
  if ! irs "$A" "$src" >"$a.ll" 2>"$a.err" || ! irs "$B" "$src" >"$b.ll" 2>"$b.err"; then
    echo "$rel FAIL"
  elif cmp -s "$a.ll" "$b.ll"; then
    echo "$rel same"
  else
    local ia ib same=same
    VELT_STD=$A/std VELT_RT_LIB=$A/libvelt_rt.a "$A/velt" build --release "$src" -o "$a.exe" >/dev/null 2>&1
    VELT_STD=$B/std VELT_RT_LIB=$B/libvelt_rt.a "$B/velt" build --release "$src" -o "$b.exe" >/dev/null 2>&1
    ia=$(ir_count "$a.exe"); ib=$(ir_count "$b.exe")
    cmp -s "$a.exe.out" "$b.exe.out" || same=DIFF
    echo "$rel diff $ia $ib $(python3 -c "print(f'{($ib/$ia-1)*100:+.3f}%')") $same"
  fi
  find "$W" -maxdepth 1 -name "$name.*" ! -name '*.ll' -type f -exec unlink {} \;
}
export -f irs ir_count one

programs=("$ROOT"/bench/*.velt "$ROOT"/bench/iter/*.velt "$ROOT"/bench/json/*.velt
  "$ROOT"/bench/strings_utf16/*.velt "$ROOT"/bench/stdport/*.velt "$ROOT"/bench/spread_order/*.velt
  "$ROOT"/bench/fetch/loopback.velt)
for name in await_chain await_deep hot_loop fanout_all fanout_all_throwing all_small_stored \
  spawn_many channel_pipeline timers_fire timers_due_join timers_due_stored timers_due_in_promise \
  timers_due_reversed top_level_await timer_await timer_await_top promise_identity_loop; do
  programs+=("$ROOT/bench/async/$name.velt")
done
printf '%s\n' "${programs[@]}" | grep -vE '/(package\.velt|_[^/]*)$' \
  | xargs -P "${JOBS:-4}" -I{} bash -c 'one {}' | tee "$W/ir_ab.txt"
awk '{ n[$2]++ } END { print "same: " n["same"]+0 ", differ: " n["diff"]+0 ", failed: " n["FAIL"]+0 }' "$W/ir_ab.txt"
