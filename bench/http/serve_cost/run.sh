#!/usr/bin/env bash
# The instructions one `serve` handler shape costs per request, against a base (cachegrind `I refs`):
# (instructions at 7,600 requests - at 1,600) / 6,000, on one keep-alive connection, base and head
# interleaved, each side's std and http-server with one compiler (std is read at compile time).
# Usage: bench/http/serve_cost/run.sh <velt> <base checkout> <head checkout> <row> [runs]
#   <row>: plain, split (a body in a packet of its own), signal, signalwait, wait, stream, generator
# Prints each run and the medians. Needs valgrind, curl and python3. Only stops what it started.
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
VELT=$1 BASE=$2 HEAD=$3 ROW=$4 RUNS=${5:-5}
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
trap 'exit 143' INT TERM

build() { # side checkout: this folder, against that checkout's http-server and std (a base has
  # neither this folder nor its lock), laid out as in the repository
  local dir="$WORK/$1/bench/http/serve_cost"
  mkdir -p "$dir" "$WORK/$1/packages/http-server"
  tar -C "$2/packages/http-server" --exclude=./target -cf - . |
    tar -C "$WORK/$1/packages/http-server" -xf -
  cp "$HERE/package.velt" "$HERE/server.velt" "$dir/"
  (cd "$dir" && VELT_STD="$2/std" "$VELT" build --release server.velt -o "$WORK/srv-$1") \
    >/dev/null 2>"$WORK/build-$1" || { cat "$WORK/build-$1"; exit 1; }
}

total() { # exe n: the instructions the server ran in all; empty if it lost its port
  local exe=$1 n=$2 port=$((12000 + RANDOM % 8000))
  VELT_THREADS=1 valgrind --vgdb=no --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
    "$exe" "$port" >/dev/null 2>"$WORK/cg" &
  local pid=$!
  for _ in $(seq 400); do
    curl -s -o /dev/null "http://127.0.0.1:$port/" && break
    kill -0 "$pid" 2>/dev/null || return
    sleep 0.25
  done
  python3 "$HERE/client.py" "$port" "$ROW" "$n" >&2
  kill -TERM "$pid"
  wait "$pid" 2>/dev/null
  grep -oE "I\s+refs:\s+[0-9,]+" "$WORK/cg" | tr -d ', ' | sed 's/Irefs://'
}

one() { # exe: instructions per request
  local a b _
  for _ in 1 2 3 4; do
    a=$(total "$1" 1600)
    b=$(total "$1" 7600)
    if [ -n "$a" ] && [ -n "$b" ] && [ $((b - a)) -gt 6000000 ]; then
      echo $(((b - a) / 6000))
      return
    fi
  done
  echo "no measurement: the server kept losing its port" >&2
  exit 1
}

build base "$BASE"
build head "$HEAD"
bases=()
heads=()
for r in $(seq "$RUNS"); do
  x=$(one "$WORK/srv-base")
  y=$(one "$WORK/srv-head")
  echo "$ROW run$r base=$x head=$y"
  bases+=("$x")
  heads+=("$y")
done
python3 - "$ROW" "${bases[*]}" "${heads[*]}" <<'PY'
import statistics, sys
b = [int(x) for x in sys.argv[2].split()]
h = [int(x) for x in sys.argv[3].split()]
mb, mh = statistics.median(b), statistics.median(h)
print(f"{sys.argv[1]}: runs {len(b)}/{len(h)} base {mb:.0f} (spread {max(b) - min(b)}) "
      f"head {mh:.0f} (spread {max(h) - min(h)}) {100 * (mh - mb) / mb:+.1f}%")
PY
