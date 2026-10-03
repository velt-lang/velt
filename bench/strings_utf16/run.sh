#!/usr/bin/env bash
# Non-ASCII string benchmark (issue #377): builds every bench/strings_utf16/*.vlt with Velt's two
# release backends, runs the Node equivalent, and prints a Markdown table of the best wall-clock
# time (ms) over RUNS runs. "output matches Node" is informational: until #377 phase 2 gives Velt
# strings JavaScript's UTF-16 semantics, lengths, indexes and sort order differ on non-ASCII text.
#
#   bench/strings_utf16/run.sh [runs] [only-part]
#
# Needs: cargo, node, python3 (for timing) on PATH; clang for the LLVM column.
set -euo pipefail
RUNS=${1:-5}
ONLY=${2:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT="$ROOT/target/bench-strings-utf16"
mkdir -p "$OUT"

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
PYTHON=$(command -v python3 || command -v python)
# velt is where cargo put it: CARGO_TARGET_DIR, a cargo config, or <repo>/target.
VELT=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')/release/velt

# best_ms <output-file> <command...>: prints the best time and saves the output of the last run.
best_ms() {
  local output=$1; shift
  "$PYTHON" - "$RUNS" "$output" "$@" <<'PY'
import subprocess, sys, time
runs, output, cmd = int(sys.argv[1]), sys.argv[2], sys.argv[3:]
best = float("inf")
for _ in range(runs):
    t = time.perf_counter()
    out = subprocess.run(cmd, check=True, capture_output=True).stdout
    best = min(best, time.perf_counter() - t)
open(output, "wb").write(out)
print(round(best * 1000))
PY
}

echo "| part | Velt cranelift release | Velt LLVM release | Node | output matches Node |"
echo "|---|---:|---:|---:|---|"
for src in "$HERE"/*.vlt; do
  name=$(basename "$src" .vlt)
  [[ -n "$ONLY" && "$name" != "$ONLY" ]] && continue
  node_ms=$(best_ms "$OUT/$name.node.out" node "$HERE/$name.js")
  row="| $name"
  matches=yes
  for backend in cranelift llvm; do
    exe="$OUT/$name-$backend"
    if "$VELT" build --release --backend "$backend" "$src" -o "$exe" 2>/dev/null &&
      ms=$(best_ms "$OUT/$name.$backend.out" "$exe"); then
      row="$row | $ms"
      cmp -s "$OUT/$name.$backend.out" "$OUT/$name.node.out" || matches=no
    else
      row="$row | n/a"
    fi
  done
  echo "$row | $node_ms | $matches |"
done
echo
echo "Best of $RUNS runs, wall-clock milliseconds including process start."
