#!/bin/sh
# Wall time of the debug-mode golden runs, the ones under the checking allocator
# (VELT_RT_DEBUG_ALLOC=1): how much a change to crates/velt_rt/src/debug_alloc* costs the suite.
# Run it from the repository root on the base and on the branch, on the same machine, with the
# test binary already built (`cargo test -p veltc --test golden --no-run`).
# Usage: bench/debug_goldens_time.sh <label> [log]
label=${1:?usage: debug_goldens_time.sh <label> [log]}
log=${2:-golden-$label.log}
start=$(date +%s)
VELT_GOLDEN_MODES=debug cargo test -p veltc --test golden > "$log" 2>&1
rc=$?
end=$(date +%s)
echo "$label: exit $rc, $((end - start)) s"
