#!/usr/bin/env bash
# Time per query of bench/sqlite/rows.vlt against rust/ (rusqlite + serde_json):
#
#   bench/sqlite/compare.sh            (VELT: a release velt, default target/release/velt)
#
# Each program runs its query 0 and N times (`feed`: N reads of 20 posts; `post`: 10 × N reads
# of one post), five times each; the best of each, the first subtracted from the second, divided
# by the queries run, is the time per query without process start and setup. The output's
# `out=` (the bytes written) must be the same for both programs. Env: N (20000), VELT, VELT_STD
# (this checkout's std/), RUST_TARGET_DIR (bench/sqlite/rust/target).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
velt="${VELT:-$root/target/release/velt}"
export VELT_STD="${VELT_STD:-$root/std}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
"$velt" build --release "$here/rows.vlt" -o "$work/rows-velt" >/dev/null
rust_target="${RUST_TARGET_DIR:-$here/rust/target}"
(cd "$here/rust" && CARGO_TARGET_DIR="$rust_target" cargo build -q --release)
rust="$rust_target/release/sqlite_rows_bench"

# GNU date has nanoseconds; elsewhere (macOS) python3's clock.
if [ "$(date +%N)" != "N" ]; then
  now_us() { echo $(($(date +%s%N) / 1000)); }
else
  now_us() { python3 -c 'import time; print(time.time_ns() // 1000)'; }
fi

# The fastest of five runs of the command, in µs.
best() {
  local m=-1
  for _ in 1 2 3 4 5; do
    local t0 t
    t0="$(now_us)"
    "$@" >/dev/null
    t=$(($(now_us) - t0))
    if [ "$m" -lt 0 ] || [ "$t" -lt "$m" ]; then m=$t; fi
  done
  echo "$m"
}

n="${N:-20000}"
for q in feed post; do
  per=$n
  [ "$q" = post ] && per=$((n * 10))
  for b in "$work/rows-velt" "$rust"; do
    a="$(best "$b" "$q" 0)"
    z="$(best "$b" "$q" "$n")"
    echo "$q $(basename "$b"): $(((z - a) * 1000 / per)) ns/query  out=$("$b" "$q" "$n")"
  done
done
