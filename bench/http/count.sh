#!/usr/bin/env bash
# The instructions an HTTP server executes per request (cachegrind `I refs`, no PMU needed, so
# it works in WSL and VMs; two runs agree within about 0.1%), for comparing two builds of
# Velt's server stack.
# Usage: bench/http/count.sh <techempower-server> <web-server>
#   The two servers are release builds of bench/http/techempower/server.vlt and
#   bench/web/velt/server.vlt (each takes its port as the first argument); pass "-" to skip one.
# Knobs: N (4000 requests per scenario), RUNS (2), SCENARIOS (all of them, as
#        server:mode:path:connections; mode `ab` is keep-alive `ab -k`, `pipe` one connection
#        pipelined 16 deep, as wrk's pipeline.lua).
# Needs valgrind, ab (apache2-utils), curl and python3. Each scenario runs the server under
# cachegrind with VELT_THREADS=1, sends N requests, and stops it; the instructions of a run
# without requests (start-up, one readiness request, shutdown) are subtracted, and the rest is
# divided by N. Prints one line per run: `<server> <mode> <path> c=<conns> run<r> <per request>`.
# Only stops the servers it started.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
N="${N:-4000}"
RUNS="${RUNS:-2}"
SCENARIOS="${SCENARIOS:-techempower:ab:/plaintext:1 techempower:ab:/json:1 techempower:ab:/fortunes:1 techempower:ab:/plaintext:32 techempower:pipe:/plaintext:1 web:ab:/plaintext:1 web:ab:/json:1 web:ab:/plaintext:32 web:pipe:/plaintext:1}"
declare -A EXE=([techempower]="${1:?usage: count.sh <techempower-server> <web-server>}" [web]="${2:?usage: count.sh <techempower-server> <web-server>}")
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# total <exe> <mode> <path> <n> <conns>: the instructions the server executed in all.
total() {
  local exe=$1 mode=$2 path=$3 n=$4 c=$5
  local port=$((20000 + RANDOM % 20000))
  VELT_THREADS=1 valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
    "$exe" "$port" >/dev/null 2>"$WORK/cg" &
  local pid=$!
  for _ in $(seq 200); do
    curl -s -o /dev/null "http://127.0.0.1:$port/plaintext" && break
    sleep 0.5
  done
  if [ "$n" -gt 0 ]; then
    if [ "$mode" = pipe ]; then
      python3 "$HERE/pipeline.py" "$port" "$path" "$n" >&2 || echo "pipelining failed" >&2
    else
      ab -q -k -n "$n" -c "$c" "http://127.0.0.1:$port$path" >"$WORK/ab" 2>&1
      grep -E "Failed requests|Non-2xx" "$WORK/ab" | grep -vE ":\s+0$" >&2
    fi
  fi
  kill -TERM "$pid"
  wait "$pid" 2>/dev/null
  grep -oE "I\s+refs:\s+[0-9,]+" "$WORK/cg" | tr -d ', ' | sed 's/Irefs://'
}

declare -A IDLE
for sc in $SCENARIOS; do
  IFS=: read -r srv mode path c <<<"$sc"
  exe=${EXE[$srv]}
  [ "$exe" = - ] && continue
  if [ -z "${IDLE[$srv]:-}" ]; then
    IDLE[$srv]=$(total "$exe" ab /plaintext 0 1)
    echo "$srv idle ${IDLE[$srv]}"
  fi
  idle=${IDLE[$srv]}
  for r in $(seq "$RUNS"); do
    all=$(total "$exe" "$mode" "$path" "$N" "$c")
    echo "$srv $mode $path c=$c run$r $(((all - idle) / N))"
  done
done
