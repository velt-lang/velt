#!/usr/bin/env bash
# TechEmpower Framework Benchmarks tests (JSON, plaintext, db, queries, fortunes, updates) for
# the Velt, Node, Node-cluster, Bun, Go and Rust servers in bench/web. See bench/web/README.md.
#
# Usage: bench/web/run.sh   (needs wrk, python3, the toolchains of the chosen SERVERS and a
#                            database from bench/web/db/db.sh up)
# Knobs (environment):
#   SERVERS         velt node node-cluster bun go rust
#   TESTS           json plaintext db queries fortunes updates
#   QUICK=1         5 s runs, 2 s warm-up and fewer levels (a smoke run; numbers mean little)
#   DURATION        seconds per measurement (15; QUICK 5)      WARMUP  seconds (5; QUICK 2)
#   CONC            connections for json/db/fortunes (16 64 256 512; QUICK 64 512)
#   CONC_PLAINTEXT  connections for plaintext (256 1024; QUICK 256), pipelined PIPELINE deep (16)
#   QUERY_COUNTS    N for queries/updates (1 5 10 15 20; QUICK 1 20) at QUERY_CONNS (512)
#   THREADS         wrk threads (min(cores, 8))
#   PGPORT          database port (5432), or DATABASE_URL for another server
#   DB_POOL         connections per server process (2 × cores; node-cluster splits it)
#   VELT            the velt compiler (target/release/velt, target/debug/velt, then PATH);
#                   VELT_STD defaults to this checkout's std/
#   RUST_TARGET_DIR cargo target dir for the Rust server (bench/web/rust/target)
#   OUT             results file prefix (bench/web/results/run-<timestamp>): writes
#                   OUT.jsonl (one JSON object per measurement) and OUT.md (table rows)
# Only kills the servers it started.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
CORES="$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu)"

if [ "${QUICK:-0}" = "1" ]; then
  DURATION="${DURATION:-5}"
  WARMUP="${WARMUP:-2}"
  CONC="${CONC:-64 512}"
  CONC_PLAINTEXT="${CONC_PLAINTEXT:-256}"
  QUERY_COUNTS="${QUERY_COUNTS:-1 20}"
else
  DURATION="${DURATION:-15}"
  WARMUP="${WARMUP:-5}"
  CONC="${CONC:-16 64 256 512}"
  CONC_PLAINTEXT="${CONC_PLAINTEXT:-256 1024}"
  QUERY_COUNTS="${QUERY_COUNTS:-1 5 10 15 20}"
fi
SERVERS="${SERVERS:-velt node node-cluster bun go rust}"
TESTS="${TESTS:-json plaintext db queries fortunes updates}"
QUERY_CONNS="${QUERY_CONNS:-512}"
PIPELINE="${PIPELINE:-16}"
THREADS="${THREADS:-$((CORES < 8 ? CORES : 8))}"
PGPORT="${PGPORT:-5432}"
export DATABASE_URL="${DATABASE_URL:-postgres://benchmarkdbuser:benchmarkdbpass@127.0.0.1:$PGPORT/hello_world}"
export DB_POOL="${DB_POOL:-$((CORES * 2))}"
export VELT_STD="${VELT_STD:-$ROOT/std}"
case "$VELT" in
  */debug/*) echo "warning: $VELT is a debug build: it links the debug runtime (debug assertions," \
    "checked allocator), so Velt numbers are far too low; use target/release/velt" >&2 ;;
esac
RUST_TARGET_DIR="${RUST_TARGET_DIR:-$HERE/rust/target}"
OUT="${OUT:-$HERE/results/run-$(date +%Y%m%d-%H%M%S)}"
WORK="$(mktemp -d)"
PIDS=()

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    if [ -n "$pid" ]; then
      pkill -P "$pid" 2>/dev/null || true
      kill "$pid" 2>/dev/null || true
    fi
  done
  rm -rf "$WORK"
}
trap cleanup EXIT

# wrk and the servers need a descriptor per connection (macOS starts at 256).
ulimit -n 65536 2>/dev/null || ulimit -n "$(ulimit -Hn)" 2>/dev/null || true

find_velt() {
  if [ -n "${VELT:-}" ]; then echo "$VELT"; return; fi
  for c in "$ROOT/target/release/velt" "$ROOT/target/debug/velt"; do
    if [ -x "$c" ]; then echo "$c"; return; fi
  done
  command -v velt || { echo "no velt compiler: set VELT" >&2; exit 1; }
}

build() {
  case "$1" in
    velt) "$(find_velt)" build --release "$HERE/velt/server.vlt" -o "$WORK/velt-server" ;;
    node | node-cluster)
      [ -d "$HERE/node/node_modules" ] || (cd "$HERE/node" && npm ci --no-audit --no-fund) ;;
    bun) ;;
    go) (cd "$HERE/go" && go build -o "$WORK/go-server" .) ;;
    rust) (cd "$HERE/rust" && CARGO_TARGET_DIR="$RUST_TARGET_DIR" cargo build --release -q) ;;
    *) echo "unknown server $1" >&2; exit 2 ;;
  esac
}

# Starts server $1 on port $2 in the background; sets STARTED_PID.
start() {
  local log="$WORK/$1.log"
  case "$1" in
    velt) "$WORK/velt-server" "$2" >"$log" 2>&1 & ;;
    node) node "$HERE/node/server.mjs" "$2" >"$log" 2>&1 & ;;
    node-cluster) CLUSTER=1 node "$HERE/node/server.mjs" "$2" >"$log" 2>&1 & ;;
    bun) bun "$HERE/bun/server.ts" "$2" >"$log" 2>&1 & ;;
    go) "$WORK/go-server" "$2" >"$log" 2>&1 & ;;
    rust) "$RUST_TARGET_DIR/release/web-bench-axum" "$2" >"$log" 2>&1 & ;;
  esac
  STARTED_PID=$!
  PIDS+=("$STARTED_PID")
  for _ in $(seq 1 100); do
    curl -sf "http://127.0.0.1:$2/plaintext" >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  echo "server $1 did not start:" >&2
  tail -20 "$log" >&2
  return 1
}

stop() {
  pkill -P "$1" 2>/dev/null || true
  kill "$1" 2>/dev/null || true
  wait "$1" 2>/dev/null || true
}

random_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}

# wrk run: $1 url, $2 connections, $3 seconds, $4 pipeline depth (1 = none).
run_wrk() {
  local threads=$((THREADS < $2 ? THREADS : $2))
  if [ "$4" -gt 1 ]; then
    wrk -t "$threads" -c "$2" -d "${3}s" --latency -s "$HERE/pipeline.lua" "$1" -- "$4"
  else
    wrk -t "$threads" -c "$2" -d "${3}s" --latency "$1"
  fi
}

# One measurement: $1 server, $2 server pid, $3 test, $4 path, $5 connections, $6 depth, $7 N.
measure() {
  local url="http://127.0.0.1:$PORT$4" sampler rps avg p99 errors non2xx rss
  run_wrk "$url" "$5" "$WARMUP" "$6" >/dev/null 2>&1 || true
  python3 "$HERE/measure.py" rss "$2" >"$WORK/rss" &
  sampler=$!
  run_wrk "$url" "$5" "$DURATION" "$6" >"$WORK/wrk.txt" 2>&1 || true
  kill "$sampler" 2>/dev/null || true
  wait "$sampler" 2>/dev/null || true
  rss="$(cat "$WORK/rss")"
  read -r rps avg p99 errors non2xx < <(python3 "$HERE/measure.py" wrk <"$WORK/wrk.txt")
  printf '{"server":"%s","test":"%s","connections":%s,"pipeline":%s,"queries":%s,"duration_s":%s,"rps":%s,"avg_ms":%s,"p99_ms":%s,"errors":%s,"non2xx":%s,"peak_rss_mb":%s}\n' \
    "$1" "$3" "$5" "$6" "${7:-null}" "$DURATION" "$rps" "$avg" "$p99" "$errors" "$non2xx" "$rss" >>"$OUT.jsonl"
  local row
  row="| $1 | $3 | ${7:--} | $5 | $rps | $p99 ms | $rss MB | $errors / $non2xx |"
  echo "$row" | tee -a "$OUT.md"
}

run_tests() {
  local server="$1" pid="$2" c n
  for test in $TESTS; do
    case "$test" in
      json | db | fortunes)
        for c in $CONC; do measure "$server" "$pid" "$test" "/$test" "$c" 1; done ;;
      plaintext)
        for c in $CONC_PLAINTEXT; do
          measure "$server" "$pid" plaintext /plaintext "$c" "$PIPELINE"
        done ;;
      queries | updates)
        for n in $QUERY_COUNTS; do
          measure "$server" "$pid" "$test" "/$test?queries=$n" "$QUERY_CONNS" 1 "$n"
        done ;;
      *) echo "unknown test $test" >&2; exit 2 ;;
    esac
  done
}

mkdir -p "$(dirname "$OUT")"
for server in $SERVERS; do
  echo "building $server..." >&2
  build "$server" >&2
done

{
  echo "<!-- $(uname -sm), $CORES cores, ${DURATION}s runs, wrk -t$THREADS, DB_POOL=$DB_POOL -->"
  echo "| server | test | queries | connections | req/s | p99 | peak RSS | socket errors / non-2xx |"
  echo "|---|---|---:|---:|---:|---:|---:|---:|"
} | tee -a "$OUT.md"
# Every server starts from the same database state: the previous server's updates left dead
# row versions in `world` (and dirty pages), which slowed every later server's queries.
reset_db() {
  if command -v psql >/dev/null 2>&1; then
    psql "$DATABASE_URL" -qc "VACUUM (FULL, ANALYZE) world" -c "VACUUM (ANALYZE) fortune" \
      -c "CHECKPOINT" >/dev/null || echo "warning: database reset failed" >&2
  else
    echo "warning: no psql; the database is not reset between servers" >&2
  fi
}

failed=""
for server in $SERVERS; do
  reset_db
  PORT="$(random_port)"
  start "$server" "$PORT"
  pid="$STARTED_PID"
  if python3 "$HERE/measure.py" verify "http://127.0.0.1:$PORT" "$DATABASE_URL"; then
    echo "$server: all routes verified" >&2
  else
    echo "$server: verification FAILED (measuring anyway)" >&2
    failed="$failed $server"
  fi
  run_tests "$server" "$pid"
  stop "$pid"
done
echo "results: $OUT.jsonl, $OUT.md" >&2
if [ -n "$failed" ]; then
  echo "verification failed for:$failed" >&2
  exit 1
fi
