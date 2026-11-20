#!/usr/bin/env bash
# #3289 A/B in the velt-prof VM, std only: one release compiler (#3256's top, ff566fe4f) builds
# every row against b = #3256's std (ff566fe4f) and n = the std of the yield-order-fix worktree (#3289 with
# its fixes, uncommitted). cachegrind instructions at VELT_THREADS=1 and 4; then the yield
# goldens built with n at 1 and 4 threads.
set -uo pipefail
REPO=/Users/andii/dev/velt/branches/yield-order-fix
BASE=ff566fe4f
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
rm -rf "$W/n-src"; cp -a "$W/b-src" "$W/n-src"
(cd "$REPO" && git diff --name-only "$BASE" -- std tests/golden/lang) | while read -r f; do
  if [[ -e $REPO/$f ]]; then mkdir -p "$(dirname "$W/n-src/$f")"; cp "$REPO/$f" "$W/n-src/$f"; else rm -f "$W/n-src/$f"; fi
done
(cd "$REPO" && git ls-files --others --exclude-standard -- std tests/golden/lang) | while read -r f; do
  mkdir -p "$(dirname "$W/n-src/$f")"; cp "$REPO/$f" "$W/n-src/$f"
done
echo "n differs from b in: $(cd "$W" && diff -rq b-src/std n-src/std | wc -l) std files"
ir() { # tree row threads
  local v=$1 r=$2 th=$3 exe=$W/cg/$2-$1
  if [[ ! -x $exe ]]; then
    VELT_STD=$W/$v-src/std VELT_RT_LIB=$W/tc/libvelt_rt.a "$W/tc/velt" build --release --backend llvm "$W/$v-src/bench/async/$r.velt" -o "$exe" > "$exe.build" 2>&1 || { echo FAIL; return; }
  fi
  (cd "$W/cg" && VELT_THREADS=$th timeout 1800 valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$exe" < /dev/null > "$exe-$th.out" 2> "$exe-$th.vg") || true
  grep -oP 'I\s+refs:\s+\K[0-9,]+' "$exe-$th.vg" | tr -d , || echo NA
}
ROWS=(race_yield race_small race_select spawn_many pingpong_colocated pingpong_pinned promise_identity_loop await_chain all_small_stored timers_fire timers_fire_phases timers_due_in_promise timers_due_join timers_due_reversed timers_due_stored timer_await timer_await_top)
for th in 1 4; do
  for r in "${ROWS[@]}"; do
    bx=$(ir b $r $th); nx=$(ir n $r $th)
    same=same; cmp -s "$W/cg/$r-b-$th.out" "$W/cg/$r-n-$th.out" || same=DIFF
    python3 -c "import sys; b,c=int(sys.argv[2]),int(sys.argv[3]); print(f'{sys.argv[1]:30} {b:>15,} {c:>15,} {(c-b)/b*100:+7.2f}% {sys.argv[4]}')" "$r t$th" "$bx" "$nx" "$same" 2>/dev/null || echo "$r t$th $bx $nx $same"
  done
done
echo "== main before the 9d stack (9cb569834, runtime yieldNow) vs n, with race_yield.velt copied in"
MAIN=9cb569834
if [[ ! -x $W/tc-m/velt ]]; then
  rm -rf "$W/m-src"; mkdir -p "$W/m-src"
  git -C "$REPO" archive "$MAIN" | tar -x -C "$W/m-src"
  cp "$W/b-src/bench/async/race_yield.velt" "$W/m-src/bench/async/race_yield.velt"
  (cd "$W/m-src" && CARGO_TARGET_DIR=$W/target-m cargo build --release -q -p veltc -p velt_rt) || { echo "main build FAIL"; }
  mkdir -p "$W/tc-m"
  cp "$W/target-m/release/velt" "$W/target-m/release/libvelt_rt.a" "$W/tc-m/" && rm -rf "$W/target-m"
fi
irm() { # row threads: main's compiler, std and runtime
  local r=$1 th=$2 exe=$W/cg/$1-m
  if [[ ! -x $exe ]]; then
    VELT_STD=$W/m-src/std VELT_RT_LIB=$W/tc-m/libvelt_rt.a "$W/tc-m/velt" build --release --backend llvm "$W/m-src/bench/async/$r.velt" -o "$exe" > "$exe.build" 2>&1 || { echo FAIL; return; }
  fi
  (cd "$W/cg" && VELT_THREADS=$th timeout 1800 valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$exe" < /dev/null > "$exe-$th.out" 2> "$exe-$th.vg") || true
  grep -oP 'I\s+refs:\s+\K[0-9,]+' "$exe-$th.vg" | tr -d , || echo NA
}
for th in 1 4; do
  for r in race_yield spawn_many pingpong_colocated pingpong_pinned race_select timers_due_reversed timers_due_stored timers_due_join; do
    mx=$(irm $r $th); nx=$(ir n $r $th)
    same=same; cmp -s "$W/cg/$r-m-$th.out" "$W/cg/$r-n-$th.out" || same=DIFF
    python3 -c "import sys; b,c=int(sys.argv[2]),int(sys.argv[3]); print(f'{sys.argv[1]:30} {b:>15,} {c:>15,} {(c-b)/b*100:+7.2f}% {sys.argv[4]}')" "main>n $r t$th" "$mx" "$nx" "$same" 2>/dev/null || echo "main>n $r t$th $mx $nx $same"
  done
done
echo "== goldens with n"
for g in yield_now_immediates yield_now_root_after_turn yield_now_due_sleep yield_now_turn_passed yield_now_order yield_now_stored yield_now_stored_sync; do
  exe=$W/cg/g-$g
  VELT_STD=$W/n-src/std VELT_RT_LIB=$W/tc/libvelt_rt.a "$W/tc/velt" build --release --backend llvm "$W/n-src/tests/golden/lang/$g.velt" -o "$exe" > "$exe.build" 2>&1 || { echo "$g build FAIL"; continue; }
  for th in 1 4; do
    VELT_THREADS=$th timeout 60 "$exe" > "$exe.$th.out" 2>&1
    cmp -s "$exe.$th.out" "$W/n-src/tests/golden/lang/$g.out" && echo "$g t$th same" || echo "$g t$th DIFF"
  done
done
echo "== break check: n without dueNow (yield_now_due_sleep must differ)"
rm -rf "$W/x-src"; cp -a "$W/n-src" "$W/x-src"
sed -i 's/  if (dueNow(e, k)) {/  if (false \&\& dueNow(e, k)) {/' "$W/x-src/std/lang/hooks/sched_fire.velt"
grep -c "false && dueNow" "$W/x-src/std/lang/hooks/sched_fire.velt"
exe=$W/cg/x-due
VELT_STD=$W/x-src/std VELT_RT_LIB=$W/tc/libvelt_rt.a "$W/tc/velt" build --release --backend llvm "$W/x-src/tests/golden/lang/yield_now_due_sleep.velt" -o "$exe" > "$exe.build" 2>&1 || echo "x build FAIL"
for th in 1 4; do
  VELT_THREADS=$th timeout 60 "$exe" > "$exe.$th.out" 2>&1
  cmp -s "$exe.$th.out" "$W/n-src/tests/golden/lang/yield_now_due_sleep.out" && echo "x t$th same (BAD)" || { echo "x t$th DIFF (good)"; diff "$exe.$th.out" "$W/n-src/tests/golden/lang/yield_now_due_sleep.out" | head -6; }
done
echo "== end"
