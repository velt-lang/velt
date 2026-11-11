# Shared helpers for the HTTP step-0 benchmarks (#2729): bench/http/techempower/scaling.sh,
# bench/http/middleware/run.sh, bench/http/modlock/run.sh. Source it; Linux only (taskset, /proc).
# Knobs: SERVER_CPUS / CLIENT_CPUS (taskset -c lists; server and client on disjoint CPUs),
#        DURATION (5s), CONNS (256), RUNS (3: best req/s of the interleaved runs is reported),
#        OHA (oha binary), VELT (velt compiler).
# Only kills the processes it started (their PIDs are recorded in PIDS).

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DURATION="${DURATION:-5s}"
CONNS="${CONNS:-256}"
RUNS="${RUNS:-3}"
OHA="${OHA:-oha}"
VELT="${VELT:-$ROOT/target/release/velt}"
SERVER_PIN=()
CLIENT_PIN=()
if [ -n "${SERVER_CPUS:-}" ]; then SERVER_PIN=(taskset -c "$SERVER_CPUS"); fi
if [ -n "${CLIENT_CPUS:-}" ]; then CLIENT_PIN=(taskset -c "$CLIENT_CPUS"); fi
WORK="$(mktemp -d)"
PIDS=()

lib_cleanup() {
  for pid in "${PIDS[@]:-}"; do
    [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  done
  rm -rf "$WORK"
}
trap lib_cleanup EXIT

random_port() {
  python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}

load1() { cut -d' ' -f1 /proc/loadavg; }

# Waits until http://127.0.0.1:$1$2 answers (up to $3 tenths of a second, default 100).
wait_ready() {
  for _ in $(seq 1 "${3:-100}"); do
    curl -sf "http://127.0.0.1:$1$2" >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  echo "server on port $1 did not start" >&2
  return 1
}

# start_server <ready-path> <cmd...>: runs the command (pinned to SERVER_CPUS) in the background;
# the command line must contain the port. Sets STARTED_PID. The port is taken from $PORT.
start_server() {
  local path="$1"
  shift
  ${SERVER_PIN[@]+"${SERVER_PIN[@]}"} "$@" >"$WORK/server.log" 2>&1 &
  STARTED_PID=$!
  PIDS+=("$STARTED_PID")
  wait_ready "$PORT" "$path" "${READY_TENTHS:-100}"
}

stop_server() {
  kill "${2:-TERM}" "$1" 2>/dev/null || kill "$1" 2>/dev/null || true
  wait "$1" 2>/dev/null || true
}

# oha JSON summary -> "req/s p99_ms success_rate" for a DURATION run at CONNS connections.
measure() {
  ${CLIENT_PIN[@]+"${CLIENT_PIN[@]}"} "$OHA" -z "$DURATION" -c "${2:-$CONNS}" --no-tui \
    --output-format json "$1" |
    python3 -c 'import json, sys
d = json.load(sys.stdin)
s, p = d["summary"], d["latencyPercentiles"]
print("%.0f %.2f %s" % (s["requestsPerSec"], (p["p99"] or 0) * 1000, s["successRate"]))'
}

warm() {
  ${CLIENT_PIN[@]+"${CLIENT_PIN[@]}"} "$OHA" -z 1s -c "${2:-$CONNS}" --no-tui \
    --output-format quiet "$1" >/dev/null 2>&1 || true
}

# Exactly $2 requests on $3 connections (default 8).
fire() {
  ${CLIENT_PIN[@]+"${CLIENT_PIN[@]}"} "$OHA" -n "$2" -c "${3:-8}" --no-tui \
    --output-format quiet "$1" >/dev/null 2>&1
}

# Peak and current RSS of a process in KiB: "VmHWM VmRSS".
rss_kib() {
  awk '/^VmHWM/ {h=$2} /^VmRSS/ {r=$2} END {print h, r}' "/proc/$1/status"
}

# Instructions (cachegrind Ir) per request of a server: runs it under cachegrind twice, serving
# N1 and N2 requests on 8 connections, and prints (Ir(N2) - Ir(N1)) / (N2 - N1), so startup and
# shutdown cancel out. Usage: ir_per_request <url-path> <N1> <N2> <cmd...> (port in $PORT).
ir_per_request() {
  local path="$1" n1="$2" n2="$3"
  shift 3
  local irs=()
  for n in "$n1" "$n2"; do
    local out="$WORK/cg.$n.$RANDOM"
    READY_TENTHS=600 start_server "$path" valgrind --tool=cachegrind --cache-sim=no --trace-children=yes \
      --cachegrind-out-file="$out" "$@"
    local pid="$STARTED_PID"
    fire "http://127.0.0.1:$PORT$path" "$n" 8
    stop_server "$pid" INT
    for _ in $(seq 1 100); do [ -s "$out" ] && grep -q '^summary:' "$out" && break; sleep 0.1; done
    irs+=("$(awk '/^summary:/ {print $2}' "$out")")
  done
  python3 -c "import sys; a, b = int(sys.argv[1]), int(sys.argv[2]); print(round((b - a) / ($n2 - $n1)))" \
    "${irs[0]}" "${irs[1]}"
}

# Per-thread CPU of process $1 from /proc: "tid ticks" lines (utime + stime).
thread_ticks() {
  for t in /proc/"$1"/task/*; do
    awk -v tid="${t##*/}" '{ sub(/^.*\) /, ""); print tid, $12 + $13 }' "$t/stat" 2>/dev/null
  done
}

# Busy share of each thread between two thread_ticks snapshots over $3 seconds, busiest first,
# threads under 2% left out: "tid:NN%" words.
thread_busy() {
  python3 - "$1" "$2" "$3" "$(getconf CLK_TCK)" <<'PY'
import sys
a = dict(l.split() for l in open(sys.argv[1]) if l.strip())
b = dict(l.split() for l in open(sys.argv[2]) if l.strip())
secs, hz = float(sys.argv[3]), float(sys.argv[4])
busy = sorted(((int(v) - int(a.get(k, 0))) / hz / secs * 100 for k, v in b.items()), reverse=True)
print(" ".join("%.0f%%" % x for x in busy if x >= 2) or "-")
PY
}
