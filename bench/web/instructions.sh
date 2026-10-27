#!/usr/bin/env bash
# CPU instructions per request of a server, counted by callgrind (Linux, valgrind):
#
#   bench/web/instructions.sh SERVER_BIN [ROUTE...]       (default routes: /json /plaintext)
#
# For each route it starts SERVER_BIN under `valgrind --tool=callgrind`, sends WARM requests one
# after another on one keep-alive connection (bench/web/requests.py), zeroes the counts
# (`callgrind_control -z`), sends N more, and dumps them (`callgrind_control -d`): the
# instructions of all the server's threads over those N requests, divided by N. Startup and
# warm-up are left out, and the figure is the same in every run (no timing noise). Env: N
# (2000), WARM (200), PORT (8089), and what the server reads (DATABASE_URL, DB_POOL) for routes
# that use the database.
#
# How the description of #1533 compared velt-native on public APIs with the private runtime
# calls before them: build bench/web/velt-native/server.vlt at both commits with a release
# `velt` of each, then run this script on both binaries.
set -euo pipefail
if [ $# -lt 1 ]; then
  echo "usage: bench/web/instructions.sh SERVER_BIN [ROUTE...]" >&2
  exit 2
fi
here="$(cd "$(dirname "$0")" && pwd)"
bin="$1"
shift
routes=("$@")
[ ${#routes[@]} -gt 0 ] || routes=(/json /plaintext)
n="${N:-2000}"
warm="${WARM:-200}"
port="${PORT:-8089}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for route in "${routes[@]}"; do
  out="$work/callgrind.out"
  rm -f "$out"*
  valgrind --tool=callgrind --callgrind-out-file="$out" "$bin" "$port" >"$work/server.log" 2>&1 &
  pid=$!
  python3 "$here/requests.py" "$port" "$route" "$warm"
  callgrind_control -z "$pid" >/dev/null
  python3 "$here/requests.py" "$port" "$route" "$n"
  callgrind_control -d "$pid" >/dev/null
  kill -9 "$pid"
  wait "$pid" 2>/dev/null || true
  total="$(awk '/^totals:/ {print $2}' "$out".1)"
  echo "$route: $((total / n)) instructions per request ($n requests)"
done
