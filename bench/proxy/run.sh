#!/usr/bin/env bash
# Typed proxies (#823): bench/proxy/proxy_signal.vlt in Velt (LLVM release) against Node 22
# running the same file as TypeScript, and the per-type microbenchmark (a type outside the
# proxiable set against the same type inside it). Prints best wall-clock ms over RUNS runs and,
# when valgrind is on PATH, instruction counts (VELT_THREADS=1, cachegrind).
#
#   bench/proxy/run.sh [runs] [velt]
set -euo pipefail
RUNS=${1:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
VELT=${2:-}
if [ -z "$VELT" ]; then
  cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
  VELT="${CARGO_TARGET_DIR:-$ROOT/target}/release/velt"
fi
OUT=$(mktemp -d)
PYTHON=$(command -v python3 || command -v python)

best_ms() {
  "$PYTHON" - "$RUNS" "$@" <<'PY'
import subprocess, sys, time
runs, cmd = int(sys.argv[1]), sys.argv[2:]
best = float("inf")
for _ in range(runs):
    t = time.perf_counter()
    subprocess.run(cmd, check=True, capture_output=True)
    best = min(best, time.perf_counter() - t)
print(round(best * 1000))
PY
}

instr() {
  if command -v valgrind >/dev/null; then
    VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$@" 2>&1 >/dev/null |
      sed -n 's/.*I *refs: *//p' | tr -d ','
  else
    echo "-"
  fi
}

for p in proxy_signal proxy_tick_plain proxy_tick_proxiable; do
  "$VELT" build --release --backend llvm "$HERE/$p.vlt" -o "$OUT/$p" >/dev/null
done
cp "$HERE/proxy_signal.vlt" "$OUT/proxy_signal.ts"
v=$("$OUT/proxy_signal")
n=$(node --no-warnings --experimental-strip-types "$OUT/proxy_signal.ts")
if [ "$v" != "$n" ]; then
  echo "proxy_signal: Velt printed '$v', Node '$n'" >&2
  exit 1
fi
echo "| program | Velt LLVM release ms | Node 22 ms | Velt instructions |"
echo "|---|---:|---:|---:|"
echo "| proxy_signal | $(best_ms "$OUT/proxy_signal") | $(best_ms node --no-warnings --experimental-strip-types "$OUT/proxy_signal.ts") | $(instr "$OUT/proxy_signal") |"
for p in proxy_tick_plain proxy_tick_proxiable; do
  echo "| $p | $(best_ms "$OUT/$p") | - | $(instr "$OUT/$p") |"
done
rm -rf "$OUT"
