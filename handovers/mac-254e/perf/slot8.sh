#!/bin/bash
# #3583 r2: the share_orig_handed test on head, with the HUB_HERE swap, and on main's std.
set -u
W=$HOME/dev/velt/branches/fix-3420-254e
P=$HOME/dev/velt/branches/r3420-254e/perf
cd "$W" || exit 1
export CARGO_TARGET_DIR=$W/target CARGO_BUILD_JOBS=3 CARGO_INCREMENTAL=0
T=std_scheduler_held
F=std/lang/hooks/sched_share.velt
echo "=== $(git log --oneline -1 | cut -c1-80) $(date +%T)"
echo "== build $(date +%T)"
cargo test -q -p veltc --test $T --no-run 2>&1 | tail -3
echo "== head $(date +%T)"
VELT_STD=$W/std cargo test -q -p veltc --test $T 2>&1 | grep -E 'test result|FAILED|panicked|left' | cut -c1-160
echo "== swap $(date +%T)"
cp "$F" "$P/sched_share.keep"
sed -i '' 's/    if (m.ld(n + nd.N_TASK) === task) {/    if (m.ld(n + nd.N_TASK) === task || m.ld(n + nd.N_TASK) === 0) {/' "$F"
echo "swap lines: $(diff "$P/sched_share.keep" "$F" | grep -c '^>')"
VELT_STD=$W/std cargo test -q -p veltc --test $T 2>&1 | grep -E 'test result|FAILED|panicked|left|blocks' | cut -c1-200
cp "$P/sched_share.keep" "$F"
echo "== main std $(date +%T)"
rm -rf "$P/base" && mkdir -p "$P/base" && git archive 60b989f58 std | tar -x -C "$P/base"
VELT_STD=$P/base/std cargo test -q -p veltc --test $T 2>&1 | grep -E 'test result|FAILED|panicked|left|blocks' | cut -c1-200
echo "== restored: $(git diff --stat | tail -1)"
echo "=== done $(date +%T)"
