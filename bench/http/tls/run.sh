#!/usr/bin/env bash
# HTTPS /plaintext (bench/http/tls/server.velt): the default build (the runtime's tokio-rustls
# listener) against the VELT_SCHEDULER=std build (the TLS server in Velt on the reactor), one
# worker each (WORKERS). Two loads: keep-alive requests (CONNS connections) and one request per
# connection (a full TLS 1.3 handshake each, X25519 + ECDSA P-256). Reports, per server and load,
# the server's instructions per request (the difference of two runs of N1 and N2 requests, so
# start-up cancels; macOS `/usr/bin/time -l`, Linux cachegrind) and req/s (best of
# RUNS interleaved rounds of RATE_REQUESTS or RATE_HANDSHAKES requests; meaningless on a busy
# machine: compare instructions there).
# The client is h2load over HTTP/1.1 (OpenSSL: TLS 1.3; oha on macOS speaks only TLS 1.2), with
# `connection: close` for the handshake load.
# Usage: bench/http/tls/run.sh   Knobs: WORKERS, CONNS, N1, N2, RUNS, RATE_REQUESTS,
# RATE_HANDSHAKES, LOADS, VELT, H2LOAD, WORK.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
VELT="${VELT:-$ROOT/target/release/velt}"
H2LOAD="${H2LOAD:-h2load}"
WORK="${WORK:-$(mktemp -d)}"
WORKERS="${WORKERS:-1}"
CONNS="${CONNS:-16}"
N1="${N1:-2000}"
N2="${N2:-12000}"
RUNS="${RUNS:-3}"
# Requests per req/s round (h2load's -D with `connection: close` never ends, so rounds count).
RATE_REQUESTS="${RATE_REQUESTS:-100000}"
RATE_HANDSHAKES="${RATE_HANDSHAKES:-3000}"
LOADS="${LOADS:-keepalive handshake}"

echo "building into $WORK..." >&2
"$VELT" build --release "$HERE/server.velt" -o "$WORK/https-rt" >&2
VELT_SCHEDULER=std "$VELT" build --release "$HERE/server.velt" -o "$WORK/https-std" >&2

PORT=0
SERVER_PID=0
start() { # $1 binary, $2 instructions file ("" for none)
  PORT=$((20000 + RANDOM % 20000))
  if [ -n "$2" ]; then
    if [ "$(uname)" = Darwin ]; then
      VELT_THREADS=$WORKERS /usr/bin/time -l -o "$2" "$1" "$PORT" >/dev/null 2>&1 &
    else
      VELT_THREADS=$WORKERS valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$2" \
        "$1" "$PORT" >/dev/null 2>&1 &
    fi
  else
    VELT_THREADS=$WORKERS "$1" "$PORT" >/dev/null 2>&1 &
  fi
  SERVER_PID=$!
  for _ in $(seq 1 600); do
    curl -sk "https://127.0.0.1:$PORT/" >/dev/null 2>&1 && return 0
    sleep 0.05
  done
  echo "server $1 did not start" >&2
  exit 1
}
stop() {
  # Under time/perf the server is the wrapper's child: stop it, and the wrapper writes its counts.
  if pgrep -P "$SERVER_PID" >/dev/null; then
    pkill -TERM -P "$SERVER_PID" 2>/dev/null || true
  else
    kill -TERM "$SERVER_PID" 2>/dev/null || true
  fi
  wait "$SERVER_PID" 2>/dev/null || true
}
instructions() { # $1 counts file
  if [ "$(uname)" = Darwin ]; then
    awk '/instructions retired/ { print $1 }' "$1"
  else
    for _ in $(seq 1 100); do grep -q '^summary:' "$1" 2>/dev/null && break; sleep 0.1; done
    awk '/^summary:/ { print $2 }' "$1"
  fi
}
load() { # $1 keepalive|handshake, then h2load's arguments; warns about failed requests
  local kind="$1"
  shift
  local out
  if [ "$kind" = handshake ]; then
    out=$("$H2LOAD" --h1 -c "$CONNS" -H "connection: close" "$@" 2>&1)
  else
    out=$("$H2LOAD" --h1 -c "$CONNS" "$@" 2>&1)
  fi
  if ! grep -q " 0 failed, 0 errored, 0 timeout" <<<"$out"; then
    echo "warning: $kind: $(grep '^requests:' <<<"$out")" >&2
  fi
  awk '/^finished in/ { print int($4) }' <<<"$out"
}

echo "| load | server | instructions / request | req/s (best of $RUNS) |"
echo "|---|---|---:|---:|"
for kind in $LOADS; do
  ipr_rt=0
  ipr_std=0
  best_rt=0
  best_std=0
  for s in rt std; do
    c1=0
    for n in "$N1" "$N2"; do
      start "$WORK/https-$s" "$WORK/counts"
      load "$kind" -n "$n" "https://127.0.0.1:$PORT/plaintext" >/dev/null
      stop
      c2=$(instructions "$WORK/counts")
      if [ "$n" = "$N1" ]; then c1=$c2; fi
    done
    eval "ipr_$s=$(( (c2 - c1) / (N2 - N1) ))"
  done
  for _ in $(seq 1 "$RUNS"); do
    for s in rt std; do
      start "$WORK/https-$s" ""
      if [ "$kind" = handshake ]; then count=$RATE_HANDSHAKES; else count=$RATE_REQUESTS; fi
      rps=$(load "$kind" -n "$count" "https://127.0.0.1:$PORT/plaintext")
      stop
      eval "best=\$best_$s"
      if [ "$rps" -gt "$best" ]; then eval "best_$s=$rps"; fi
    done
  done
  echo "| $kind | rt | $ipr_rt | $best_rt |"
  echo "| $kind | std | $ipr_std | $best_std |"
done
