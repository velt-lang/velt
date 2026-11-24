#!/bin/bash
# #3487 wake-order (b) slot: rebuild on main, the wake and bounds tests, then arm64 rows here and
# x86 cachegrind rows in the OrbStack VM (base = main's std, new = the branch's).
set -u
W=$HOME/dev/velt/branches/fix-3420-254e
P=$HOME/dev/velt/branches/r3420-254e/perf
ROWS="chan_wake pingpong_colocated pingpong_pinned timers_fire timer_await race_select race_small race_yield spawn_many fanout_all await_chain all_small_stored promise_identity_loop channel_pipeline"
cd "$W" || exit 1
export CARGO_TARGET_DIR=$W/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0 VELT_STD=$W/std
echo "== build $(date +%T)"
cargo build -q --workspace > "$P/b4.log" 2>&1 || { echo "BUILD FAILED"; tail -20 "$P/b4.log"; exit 1; }
for t in std_scheduler_wake_order std_scheduler_bounds std_scheduler_parked_away std_scheduler_waiting_in_hub; do
  echo "$t: $(cargo test -q -p veltc --test $t 2>&1 | grep -E 'test result|FAILED' | tr '\n' ' ' | cut -c1-110)"
done
unset VELT_STD
rm -rf "$P/base" "$P/new" && mkdir -p "$P/base" "$P/new"
git archive origin/main std | tar -x -C "$P/base"
git archive HEAD std | tar -x -C "$P/new"
echo "std diff: $(diff -rq "$P/base/std" "$P/new/std" | wc -l) files"
echo "== arm64 $(date +%T)"
ROWS="$ROWS" bash "$P/mac.sh"
echo "== x86 build $(date +%T)"
orb -m velt-prof bash "$P/vm.sh" build
echo "== x86 rows $(date +%T)"
orb -m velt-prof bash "$P/vm.sh" vars "$ROWS" new
echo "== done $(date +%T)"
