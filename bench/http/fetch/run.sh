#!/usr/bin/env bash
# The fetch client benchmark: Velt's global `fetch` against Node's (undici) and Rust's reqwest,
# all calling the same local hyper server (bench/http/fetch/README.md).
# Usage: bench/http/fetch/run.sh   (needs cargo, node; a release `velt` on PATH or $VELT)
# Knobs: SCENARIOS (seq conc big json) RUNS (3) CLIENTS (velt node reqwest)
#        BENCH_TARGET_DIR (cargo target dir for the Rust parts; default: a temp dir).
# Prints one line per run: wall-clock result, CPU seconds and peak RSS (GNU time), plus the
# instructions the client executed when `perf` is available. Only kills the server it started.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
VELT="${VELT:-velt}"
SCENARIOS="${SCENARIOS:-seq conc big json}"
RUNS="${RUNS:-3}"
CLIENTS="${CLIENTS:-velt node reqwest}"
WORK="$(mktemp -d)"
TARGET="${BENCH_TARGET_DIR:-$WORK/target}"
PORT=18080
SERVER_PID=""

cleanup() {
  [ -n "$SERVER_PID" ] && kill "$SERVER_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

CARGO_TARGET_DIR="$TARGET" cargo build --release --quiet --manifest-path "$HERE/rust/Cargo.toml"
"$VELT" build --release "$HERE/client.vlt" -o "$WORK/client-velt"
"$TARGET/release/server" "$PORT" >/dev/null &
SERVER_PID=$!
sleep 1
BASE="http://127.0.0.1:$PORT"

measure() {
  if command -v perf >/dev/null 2>&1; then
    perf stat -x, -e instructions:u -o "$WORK/perf" /usr/bin/time -f "  [cpu %U+%S s, peak-rss %M KB]" "$@"
    echo "  [instructions $(cut -d, -f1 "$WORK/perf" | grep -E '^[0-9]+$' | head -1)]"
  else
    /usr/bin/time -f "  [cpu %U+%S s, peak-rss %M KB]" "$@"
  fi
}

for s in $SCENARIOS; do
  for r in $(seq "$RUNS"); do
    for c in $CLIENTS; do
      echo "== $s run $r: $c"
      case $c in
        velt) measure "$WORK/client-velt" "$BASE" "$s" ;;
        node) measure node "$HERE/client.mjs" "$BASE" "$s" ;;
        reqwest) measure "$TARGET/release/client" "$BASE" "$s" ;;
      esac
    done
  done
done
