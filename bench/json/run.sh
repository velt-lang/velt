#!/usr/bin/env bash
# JSON benchmark harness (Linux / macOS). Builds every bench/json/*.vlt (LLVM release) plus the
# Rust (serde_json) and Node versions, checks that all print the same output, and prints a
# Markdown table of the best wall-clock time (ms) over RUNS runs.
#
#   bench/json/run.sh [runs] [only-benchmark]
#
# Builds into cargo's target directory (CARGO_TARGET_DIR when set).
# Needs: cargo, node, python3 on PATH; clang for the LLVM backend.
set -euo pipefail
RUNS=${1:-5}
ONLY=${2:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
# Cargo's target directory for this workspace: CARGO_TARGET_DIR, a cargo config, or <repo>/target.
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-json"
mkdir -p "$OUT"

echo "building velt (release), the runtime and the serde_json benchmarks..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
# The Rust benchmarks are their own workspace: build them next to velt, where they are run from.
cargo build --release -q --manifest-path "$HERE/rust/Cargo.toml" --target-dir "$TARGET"
VELT="$TARGET/release/velt"

# best_ms <expected-output-file|-> <command...>: prints the best time; with "-", writes the
# output to $OUT/expected instead of checking it; fails on differing output.
best_ms() {
  local expected=$1; shift
  "$PYTHON" - "$RUNS" "$expected" "$OUT/expected" "$@" <<'PY'
import subprocess, sys, time
runs, expected, store, cmd = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4:]
best = float("inf")
for _ in range(runs):
    t = time.perf_counter()
    out = subprocess.run(cmd, check=True, capture_output=True).stdout
    best = min(best, time.perf_counter() - t)
if expected == "-":
    open(store, "wb").write(out)
elif out != open(expected, "rb").read():
    sys.exit(f"{' '.join(cmd)}: output differs from Rust")
print(round(best * 1000))
PY
}

echo "| benchmark | Velt (LLVM release) | Rust serde_json | Node |"
echo "|---|---|---|---|"
for src in "$HERE"/*.vlt; do
  name=$(basename "$src" .vlt)
  [[ -n "$ONLY" && "$name" != "$ONLY" ]] && continue
  echo "$name..." >&2
  "$VELT" build --release "$src" -o "$OUT/$name"
  rust=$(best_ms - "$TARGET/release/$name")
  exp="$OUT/$name.expected"
  mv "$OUT/expected" "$exp"
  velt=$(best_ms "$exp" "$OUT/$name")
  node=$(best_ms "$exp" node "$HERE/node/$name.js")
  echo "| $name | $velt | $rust | $node |"
done
echo
echo "Best of $RUNS runs, wall-clock milliseconds including process start."
