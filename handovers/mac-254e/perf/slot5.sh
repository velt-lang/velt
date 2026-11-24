#!/bin/bash
# #3498 slot: arm64 rows (release build here), x86 cachegrind rows in the OrbStack VM, then the
# gate. Base = the branch base 4f0ae73e0, new = the branch's std.
set -u
W=$HOME/dev/velt/branches/fix-3420-254e
P=$HOME/dev/velt/branches/r3420-254e/perf
ROWS="await_chain await_deep fanout_all promise_identity_loop spawn_many all_small_stored race_select race_small race_yield chan_wake pingpong_colocated pingpong_pinned timers_fire timer_await channel_pipeline"
cd "$W" || exit 1
rm -rf "$P/base" "$P/new" && mkdir -p "$P/base" "$P/new"
git archive 4f0ae73e0 std | tar -x -C "$P/base"
git archive HEAD std | tar -x -C "$P/new"
echo "std diff: $(diff -rq "$P/base/std" "$P/new/std" | wc -l) files"
echo "== arm64 $(date +%T)"
ROWS="$ROWS" bash "$P/mac.sh"
echo "== x86 build $(date +%T)"
orb -m velt-prof bash "$P/vm.sh" build
echo "== x86 rows $(date +%T)"
orb -m velt-prof bash "$P/vm.sh" vars "$ROWS" new
echo "== gate $(date +%T)"
export CARGO_TARGET_DIR=$W/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
scripts/check.sh > "$P/gate6.log" 2>&1
echo "gate exit $?"
grep -v "^\s*PASS" "$P/gate6.log" | tail -12
echo "== done $(date +%T)"
