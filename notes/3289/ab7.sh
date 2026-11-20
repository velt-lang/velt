#!/usr/bin/env bash
# #3289 A/B in the velt-prof VM, std only (after ab4.sh, by mac-77e0). One release compiler
# (#3256's top, ff566fe4f) builds every row against:
#   b = #3256's std (ff566fe4f)
#   n = the std of commit $NREV (default 094a4a811)
#   p = the std of the worktree (uncommitted changes included)
# and main before the 9d stack (9cb569834, its own compiler, runtime and std) builds m.
# cachegrind instructions at VELT_THREADS=1 and 4; the yield goldens with p at 1 and 4 threads.
# Usage: ab7.sh [rows...]   (default: every row)
set -uo pipefail
REPO=/Users/andii/dev/velt/branches/yield-order-fix
BASE=ff566fe4f
MAIN=9cb569834
NREV=${NREV:-094a4a811}
W=$HOME/ab-3289
mkdir -p "$W/tmp" "$W/cg"
export TMPDIR=$W/tmp CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3
if [[ ! -x $W/tc/velt ]]; then
  rm -rf "$W/b-src"; mkdir -p "$W/b-src"
  git -C "$REPO" archive "$BASE" | tar -x -C "$W/b-src"
  (cd "$W/b-src" && CARGO_TARGET_DIR=$W/target cargo build --release -q -p veltc -p velt_rt) || { echo "build FAIL"; exit 1; }
  mkdir -p "$W/tc"
  cp "$W/target/release/velt" "$W/target/release/libvelt_rt.a" "$W/tc/"
  rm -rf "$W/target"
fi
if [[ ! -x $W/tc-m/velt ]]; then
  rm -rf "$W/m-src"; mkdir -p "$W/m-src"
  git -C "$REPO" archive "$MAIN" | tar -x -C "$W/m-src"
  cp "$W/b-src/bench/async/race_yield.velt" "$W/m-src/bench/async/race_yield.velt"
  (cd "$W/m-src" && CARGO_TARGET_DIR=$W/target-m cargo build --release -q -p veltc -p velt_rt) || { echo "main build FAIL"; exit 1; }
  mkdir -p "$W/tc-m"
  cp "$W/target-m/release/velt" "$W/target-m/release/libvelt_rt.a" "$W/tc-m/" && rm -rf "$W/target-m"
fi
# n: b's tree with $NREV's std and goldens.
rm -rf "$W/n-src"; cp -a "$W/b-src" "$W/n-src"
rm -rf "$W/n-src/std" "$W/n-src/tests/golden/lang"
git -C "$REPO" archive "$NREV" std tests/golden/lang | tar -x -C "$W/n-src"
# p: b's tree with the worktree's std and goldens.
rm -rf "$W/p-src"; cp -a "$W/b-src" "$W/p-src"
rm -rf "$W/p-src/std" "$W/p-src/tests/golden/lang"
mkdir -p "$W/p-src/tests/golden"
cp -a "$REPO/std" "$W/p-src/std"
cp -a "$REPO/tests/golden/lang" "$W/p-src/tests/golden/lang"
echo "n ($NREV) differs from b in: $(cd "$W" && diff -rq b-src/std n-src/std | wc -l) std files; p (worktree) from n in: $(cd "$W" && diff -rq n-src/std p-src/std | wc -l)"
rm -f "$W"/cg/*-n "$W"/cg/*-p
ir() { # tree row threads
  local v=$1 r=$2 th=$3 exe=$W/cg/$2-$1 tc=$W/tc
  [[ $v == m ]] && tc=$W/tc-m
  if [[ ! -x $exe ]]; then
    VELT_STD=$W/$v-src/std VELT_RT_LIB=$tc/libvelt_rt.a "$tc/velt" build --release --backend llvm "$W/$v-src/bench/async/$r.velt" -o "$exe" > "$exe.build" 2>&1 || { echo FAIL; return; }
  fi
  (cd "$W/cg" && VELT_THREADS=$th timeout 1800 valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$exe" < /dev/null > "$exe-$th.out" 2> "$exe-$th.vg") || true
  grep -oP 'I\s+refs:\s+\K[0-9,]+' "$exe-$th.vg" | tr -d , || echo NA
}
pct() { python3 -c "import sys; b,c=int(sys.argv[1]),int(sys.argv[2]); print(f'{(c-b)/b*100:+6.2f}%')" "$1" "$2" 2>/dev/null || echo "   NA"; }
ROWS=(race_yield race_small race_select spawn_many pingpong_colocated pingpong_pinned promise_identity_loop await_chain all_small_stored timers_fire timers_fire_phases timers_due_in_promise timers_due_join timers_due_reversed timers_due_stored timer_await timer_await_top)
MROWS=" race_yield spawn_many pingpong_colocated pingpong_pinned race_select timers_due_reversed timers_due_stored timers_due_join timers_fire timer_await "
[[ $# -gt 0 ]] && ROWS=("$@")
printf '%-26s %14s %14s %14s %14s | %8s %8s | %8s %8s\n' row b n p m "b>n" "b>p" "m>n" "m>p"
for th in 1 4; do
  for r in "${ROWS[@]}"; do
    bx=$(ir b $r $th); nx=$(ir n $r $th); px=$(ir p $r $th)
    mx=-; mn="       -"; mp="       -"
    if [[ $MROWS == *" $r "* ]]; then mx=$(ir m $r $th); mn=$(pct "$mx" "$nx"); mp=$(pct "$mx" "$px"); fi
    same=""; cmp -s "$W/cg/$r-b-$th.out" "$W/cg/$r-p-$th.out" || same=" OUT-DIFF"
    printf '%-26s %14s %14s %14s %14s | %8s %8s | %8s %8s%s\n' "$r t$th" "$bx" "$nx" "$px" "$mx" "$(pct "$bx" "$nx")" "$(pct "$bx" "$px")" "$mn" "$mp" "$same"
  done
done
echo "== goldens with p"
for g in yield_now_immediates yield_now_root_after_turn yield_now_due_sleep yield_now_turn_passed yield_now_order yield_now_stored yield_now_stored_sync; do
  exe=$W/cg/g-$g
  VELT_STD=$W/p-src/std VELT_RT_LIB=$W/tc/libvelt_rt.a "$W/tc/velt" build --release --backend llvm "$W/p-src/tests/golden/lang/$g.velt" -o "$exe" > "$exe.build" 2>&1 || { echo "$g build FAIL"; continue; }
  for th in 1 4; do
    VELT_THREADS=$th timeout 60 "$exe" > "$exe.$th.out" 2>&1
    cmp -s "$exe.$th.out" "$W/p-src/tests/golden/lang/$g.out" && echo "$g t$th same" || echo "$g t$th DIFF"
  done
done
echo "== end"
