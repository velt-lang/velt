#!/bin/bash
# One slot job for two PRs, one after the other in the same worktree:
#   #3559 (wake-order-3487, base 4f0ae73e0): its tests, arm64 + x86 rows, the gate;
#   #3583 (let-go-3498, base 60b989f58): its tests, arm64 + x86 rows, the gate.
set -u
W=$HOME/dev/velt/branches/fix-3420-254e
P=$HOME/dev/velt/branches/r3420-254e/perf
cd "$W" || exit 1
export CARGO_TARGET_DIR=$W/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
run() { # <branch> <base> <rows> <tests> <gate log>
  git switch -q "$1" || { echo "switch $1 failed"; return; }
  echo "=== $1 at $(git log --oneline -1 | cut -c1-80) $(date +%T)"
  for t in $4; do
    echo "$t: $(VELT_STD=$W/std cargo test -q -p veltc --test "$t" 2>&1 | grep -E 'test result|FAILED' | tr '\n' ' ' | cut -c1-90)"
  done
  rm -rf "$P/base" "$P/new" && mkdir -p "$P/base" "$P/new"
  git archive "$2" std | tar -x -C "$P/base"
  git archive HEAD std | tar -x -C "$P/new"
  echo "std diff: $(diff -rq "$P/base/std" "$P/new/std" | wc -l) files"
  echo "== arm64 $(date +%T)"
  ROWS="$3" bash "$P/mac.sh" | grep -v binary
  echo "== x86 build $(date +%T)"
  orb -m velt-prof bash "$P/vm.sh" build | tail -1
  echo "== x86 rows $(date +%T)"
  orb -m velt-prof bash "$P/vm.sh" vars "$3" new
  echo "== gate $(date +%T)"
  scripts/check.sh > "$5" 2>&1
  echo "gate exit $?"
  grep -E "^\s+FAIL|Summary" "$5" | sort -u | cut -c1-140
}
run wake-order-3487 4f0ae73e0 "chan_wake pingpong_colocated pingpong_pinned timers_fire timer_await race_select race_small spawn_many fanout_all" "std_scheduler_wake_order std_scheduler_bounds" "$P/gate8.log"
run let-go-3498 60b989f58 "fanout_all all_small_stored race_small race_yield timers_fire promise_identity_loop spawn_many await_chain chan_wake pingpong_colocated" "std_scheduler_held" "$P/gate9.log"
echo "=== done $(date +%T)"
