#!/bin/bash
# #3174's queue failure on Linux (OrbStack VM velt-prof): std_scheduler_rescue
# a_line_printed_on_a_lazy_worker_comes_before_what_the_rescued_task_prints_next, 50 at once, on
# #3174's head and on main, each from its own worktree into its own target in the VM.
#   orb -m velt-prof bash notes/3174/repro.sh
set -u
source "$HOME/.cargo/env"
OUT=$HOME/r3174
T=a_line_printed_on_a_lazy_worker_comes_before_what_the_rescued_task_prints_next
mkdir -p "$OUT/logs"
for side in head main; do
  wt=/Users/andii/dev/velt/branches/serve-spread
  [ $side = main ] && wt=/Users/andii/dev/velt/branches/main-3174
  cd "$wt"
  export CARGO_TARGET_DIR=$OUT/$side CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 VELT_STD=$wt/std
  echo "== $side $(git log --oneline -1 | cut -c1-60)"
  nice -n 19 cargo test -q -p veltc --test std_scheduler_rescue --no-run 2>&1 | grep -E "^error" -A6 | head -10
  BIN=$(ls -t $CARGO_TARGET_DIR/debug/deps/std_scheduler_rescue-* | grep -v '\.d$' | head -1)
  "$BIN" --exact "$T" -q > "$OUT/logs/$side-0.log" 2>&1; echo "alone: exit $?"
  for round in 1 2; do
    pids=()
    for i in $(seq 1 50); do
      "$BIN" --exact "$T" -q > "$OUT/logs/$side-$round-$i.log" 2>&1 &
      pids+=($!)
    done
    fails=0
    for p in "${pids[@]}"; do wait "$p" || fails=$((fails + 1)); done
    echo "$side round $round: $fails of 50 failed"
  done
  grep -hE "unfreed|blocks=[0-9]+/[0-9]+" "$OUT"/logs/$side-*.log | sort | uniq -c | head -5
done
echo "== logs kept in $OUT/logs; targets removed"
rm -rf "$OUT/head" "$OUT/main"
df -h "$HOME" | tail -1
