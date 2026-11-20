#!/usr/bin/env bash
# Re-measure n (the worktree's std now) on a few rows against b (#3256's std) and m (main before
# 9d), with ab4.sh's compilers and binaries; then the yield goldens with n.
set -uo pipefail
W=$HOME/ab-3289
REPO=/Users/andii/dev/velt/branches/yield-order-fix
BASE=ff566fe4f
rm -rf "$W/n-src" "$W/cg4"; cp -a "$W/b-src" "$W/n-src"; mkdir -p "$W/cg4"
(cd "$REPO" && git diff --name-only "$BASE" -- std tests/golden/lang) | while read -r f; do
  if [[ -e $REPO/$f ]]; then mkdir -p "$(dirname "$W/n-src/$f")"; cp "$REPO/$f" "$W/n-src/$f"; else rm -f "$W/n-src/$f"; fi
done
vg() { grep -oP 'I\s+refs:\s+\K[0-9,]+' "$1" | tr -d , || echo NA; }
for r in spawn_many race_select timers_due_reversed timers_due_stored race_yield pingpong_colocated; do
  exe=$W/cg4/$r
  VELT_STD=$W/n-src/std VELT_RT_LIB=$W/tc/libvelt_rt.a "$W/tc/velt" build --release --backend llvm "$W/n-src/bench/async/$r.velt" -o "$exe" > "$exe.build" 2>&1 || { echo "$r build FAIL"; tail -5 "$exe.build"; continue; }
  for th in 1 4; do
    (cd "$W/cg4" && VELT_THREADS=$th valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$exe" < /dev/null > "$exe-$th.out" 2> "$exe-$th.vg")
    n=$(vg "$exe-$th.vg"); b=$(vg "$W/cg/$r-b-$th.vg"); mm=$(vg "$W/cg/$r-m-$th.vg")
    python3 -c "import sys; n,b,m=int(sys.argv[2]),int(sys.argv[3]),int(sys.argv[4]) if sys.argv[4]!='NA' else 0; print(f'{sys.argv[1]:26} n {n:>14,}  vs b {(n-b)/b*100:+6.2f}%' + (f'  vs main {(n-m)/m*100:+6.2f}%' if m else ''))" "$r t$th" "$n" "$b" "$mm"
  done
done
echo "== goldens with n"
for g in yield_now_immediates yield_now_root_after_turn yield_now_due_sleep yield_now_turn_passed yield_now_order yield_now_stored yield_now_stored_sync; do
  exe=$W/cg4/g-$g
  VELT_STD=$W/n-src/std VELT_RT_LIB=$W/tc/libvelt_rt.a "$W/tc/velt" build --release --backend llvm "$W/n-src/tests/golden/lang/$g.velt" -o "$exe" > "$exe.build" 2>&1 || { echo "$g build FAIL"; continue; }
  for th in 1 4; do
    VELT_THREADS=$th timeout 60 "$exe" > "$exe.$th.out" 2>&1
    cmp -s "$exe.$th.out" "$W/n-src/tests/golden/lang/$g.out" && echo "$g t$th same" || echo "$g t$th DIFF"
  done
done
echo "== n4 end"
