#!/usr/bin/env bash
# Benchmark harness (Linux / macOS): same as run.ps1. Builds every bench/*.vlt with Velt' three
# configurations plus the Rust (rustc -O) and Node equivalents, checks that all print the same
# output, and prints a Markdown table of the best wall-clock time (ms) over RUNS runs.
#
#   bench/run.sh [runs] [only-benchmark]
#
# Needs: cargo, rustc, node, python3 (for timing) on PATH; clang for the LLVM column.
# Async benchmarks (tokio baselines, peak memory) have their own harness: bench/async/run.sh.
set -euo pipefail
RUNS=${1:-5}
ONLY=${2:-}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
OUT="$ROOT/target/bench"
mkdir -p "$OUT"

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
VELT="$ROOT/target/release/velt"
PYTHON=$(command -v python3 || command -v python)

# best_ms <expected-output-file> <command...>: prints the best time; fails on differing output.
best_ms() {
  local expected=$1; shift
  "$PYTHON" - "$RUNS" "$expected" "$@" <<'EOF'
import subprocess, sys, time
runs, expected, cmd = int(sys.argv[1]), sys.argv[2], sys.argv[3:]
best = float("inf")
for _ in range(runs):
    t = time.perf_counter()
    out = subprocess.run(cmd, check=True, capture_output=True).stdout
    best = min(best, time.perf_counter() - t)
if expected != "-" and out != open(expected, "rb").read():
    sys.exit(f"{' '.join(cmd)}: output differs from Rust")
print(round(best * 1000))
EOF
}

CONFIGS=("Velt cranelift debug|" "Velt cranelift release (+velt_opt)|--release --backend cranelift" "Velt LLVM release|--release --backend llvm")
header="| benchmark"
for c in "${CONFIGS[@]}"; do header="$header | ${c%%|*}"; done
echo "$header | Rust -O | Node |"
echo "|---|---|---|---|---|---|"
for src in "$ROOT"/bench/*.vlt; do
  name=$(basename "$src" .vlt)
  [[ -n "$ONLY" && "$name" != "$ONLY" ]] && continue
  rustc -O --edition 2021 -o "$OUT/$name-rust" "$ROOT/bench/rust/$name.rs"
  "$OUT/$name-rust" > "$OUT/$name.expected"
  row="| $name"
  i=0
  for c in "${CONFIGS[@]}"; do
    exe="$OUT/$name-$i"; i=$((i + 1))
    # shellcheck disable=SC2086 # the flags are meant to split
    if "$VELT" build ${c#*|} "$src" -o "$exe" 2>/dev/null; then
      row="$row | $(best_ms "$OUT/$name.expected" "$exe")"
    else
      row="$row | n/a"
    fi
  done
  row="$row | $(best_ms - "$OUT/$name-rust")"
  row="$row | $(best_ms "$OUT/$name.expected" node "$ROOT/bench/node/$name.js") |"
  echo "$row"
done
echo
echo "Best of $RUNS runs, wall-clock milliseconds including process start."
