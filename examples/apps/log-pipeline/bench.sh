#!/usr/bin/env bash
# Velt vs Node on the same generated log: output must be identical; prints user CPU time,
# wall time and peak RSS (median of 3 runs each).
#   ./bench.sh [lines]      (default 1000000; the log is written to $TMPDIR and deleted)
set -euo pipefail
cd "$(dirname "$0")"
lines=${1:-1000000}
velt=${VELT:-../../../target/debug/velt}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
"$velt" build --release -o "$tmp/lp" >/dev/null

# run <label> <cmd...>: 3 runs, median user seconds / wall seconds / max RSS MB.
run() {
    local label=$1
    shift
    local stats=()
    for _ in 1 2 3; do
        /usr/bin/time -p -o "$tmp/time" "$@" >"$tmp/out" 2>/dev/null
        local user real
        user=$(awk '/^user/ {print $2}' "$tmp/time")
        real=$(awk '/^real/ {print $2}' "$tmp/time")
        stats+=("$user $real")
    done
    local median
    median=$(printf '%s\n' "${stats[@]}" | sort -n | sed -n 2p)
    printf '%-14s user %6ss  wall %6ss\n' "$label" ${median}
}

run "velt gen" "$tmp/lp" gen "$tmp/velt.log" --lines "$lines"
run "node gen" node node/pipeline.mjs gen "$tmp/node.log" "$lines" 42
cmp -s "$tmp/velt.log" "$tmp/node.log" && echo "generated logs identical ($(wc -c <"$tmp/velt.log") bytes)"
rm "$tmp/node.log"
run "velt report" "$tmp/lp" report "$tmp/velt.log"
cp "$tmp/out" "$tmp/velt.out"
run "node report" node node/pipeline.mjs report "$tmp/velt.log"
cmp -s "$tmp/out" "$tmp/velt.out" && echo "reports identical"
for cmd in "$tmp/lp report $tmp/velt.log" "node node/pipeline.mjs report $tmp/velt.log"; do
    /usr/bin/time -l $cmd 2>&1 >/dev/null | awk -v c="${cmd%% *}" '/maximum resident/ {printf "%-14s peak RSS %d MB\n", (c ~ /node/ ? "node report" : "velt report"), $1/1048576}'
done
