#!/bin/bash
# #3487 perf rows: arm64 on this Mac, then x86 cachegrind in the OrbStack VM velt-prof.
set -u
P=$HOME/dev/velt/branches/r3420-254e/perf
ROWS="race_select race_small race_yield await_chain fanout_all spawn_many pingpong_pinned pingpong_colocated promise_identity_loop all_small_stored timers_fire timer_await chan_wake channel_pipeline"
echo "== arm64 $(date +%T)"
ROWS="$ROWS" bash "$P/mac.sh"
echo "== x86 build $(date +%T)"
orb -m velt-prof bash "$P/vm.sh" build
echo "== x86 rows $(date +%T)"
orb -m velt-prof bash "$P/vm.sh" vars "$ROWS" new
echo "== done $(date +%T)"
