#!/usr/bin/env bash
# Instructions per operation of the release sequences with and without the weak flag
# (src/weak/tests/bench.rs), counted by valgrind (Linux; callgrind, the tool with per-function
# collection, counts the same instructions as cachegrind):
#
#   crates/velt_rt/scripts/weak_rc_cost.sh [N]
#
# Run from the repository root. Builds velt_rt's unit tests (release profile), runs the `rc_paths`
# measurement once per loop with only its function counted (`--toggle-collect`), and prints the
# instructions per iteration. N (default 1000000) is the iterations of each loop.
set -euo pipefail
N=${1:-1000000}
bin=$(cargo test -p velt_rt --lib --release --no-run --message-format=json 2>/dev/null |
  sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -1)
[ -x "$bin" ] || { echo "test binary not found" >&2; exit 1; }
out=$(mktemp -d)
for loop in plain_shared capable_shared weak_shared plain_unique capable_unique; do
  # The weak loop is the capable loop run on a weakly held object.
  fn=bench_${loop/weak_/capable_}
  VELT_WEAK_BENCH=$loop VELT_WEAK_BENCH_N=$N valgrind --tool=callgrind \
    --callgrind-out-file="$out/$loop.out" --toggle-collect="*${fn}*" \
    "$bin" --ignored --exact weak::tests::bench::rc_paths --test-threads=1 >/dev/null 2>&1
  ir=$(sed -n 's/^\(summary\|totals\): *\([0-9]*\).*/\2/p' "$out/$loop.out" | head -1)
  awk -v f="$loop" -v ir="$ir" -v n="$N" \
    'BEGIN { printf "%-15s %12d Ir  %7.2f Ir/iteration\n", f, ir, ir / n }'
done
rm -rf "$out"
