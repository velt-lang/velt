#!/usr/bin/env bash
# Async benchmark harness (Linux / macOS): same as run.ps1. Builds every bench/async/*.vlt (LLVM
# release) plus the Rust/tokio and Node versions, checks that all print the same output, and prints
# Markdown tables of the best wall-clock time (ms) over RUNS runs and of the peak RSS (MB).
#
#   bench/async/run.sh [runs] [only-benchmark]
#
# Columns: Velt on all cores and with VELT_THREADS=1, Rust tokio multi-thread and current-thread,
# Node. Honors CARGO_TARGET_DIR. A configuration whose first run takes over 10 s is not repeated.
# Needs: cargo, node, python3 on PATH; clang for the LLVM backend.
set -euo pipefail
RUNS=${1:-5}
ONLY=${2:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT="$TARGET/bench-async"
mkdir -p "$OUT"

echo "building velt (release), the runtime and the tokio benchmarks..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
cargo build --release -q --manifest-path "$HERE/rust/Cargo.toml"
VELT="$TARGET/release/velt"
PYTHON=$(command -v python3 || command -v python)

# measure <expected-output-file|-> <command...>: prints "<best ms> <peak MB>"; with "-" instead
# of an expected file, writes the output to $OUT/expected; fails on differing output.
measure() {
  local expected=$1; shift
  "$PYTHON" - "$RUNS" "$expected" "$OUT/expected" "$@" <<'EOF'
import resource, subprocess, sys, time
runs, expected, store, cmd = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4:]
best = float("inf")
for _ in range(runs):
    t = time.perf_counter()
    out = subprocess.run(cmd, check=True, capture_output=True).stdout
    elapsed = time.perf_counter() - t
    best = min(best, elapsed)
    if elapsed > 10:
        break
if expected == "-":
    open(store, "wb").write(out)
elif out != open(expected, "rb").read():
    sys.exit(f"{' '.join(cmd)}: output differs from Rust")
peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
peak_mb = peak / (1024 * 1024) if sys.platform == "darwin" else peak / 1024
print(round(best * 1000), f"{peak_mb:.1f}")
EOF
}

times="| benchmark | Velt (all cores) | Velt (1 thread) | Rust tokio multi-thread | Rust tokio current-thread | Node |
|---|---|---|---|---|---|"
mems=$times
for src in "$HERE"/*.vlt; do
  name=$(basename "$src" .vlt)
  [[ -n "$ONLY" && "$name" != "$ONLY" ]] && continue
  echo "$name..." >&2
  "$VELT" build --release "$src" -o "$OUT/$name"
  rust="$TARGET/release/$name"
  # Each result is "<ms> <MB>"; plain assignments so that a failed check stops the script.
  rust_multi=$(measure - "$rust")
  exp="$OUT/$name.expected"
  mv "$OUT/expected" "$exp"
  rust_current=$(measure "$exp" "$rust" current)
  velt_all=$(measure "$exp" "$OUT/$name")
  velt_one=$(VELT_THREADS=1 measure "$exp" "$OUT/$name")
  node=$(measure "$exp" node "$HERE/node/$name.js")
  row=("$velt_all" "$velt_one" "$rust_multi" "$rust_current" "$node")
  times="$times
| $name"
  mems="$mems
| $name"
  for r in "${row[@]}"; do
    times="$times | ${r% *}"
    mems="$mems | ${r#* }"
  done
  times="$times |"
  mems="$mems |"
done
echo "Best of $RUNS runs, wall-clock ms including process start:"
echo
echo "$times"
echo
echo "Peak RSS, MB:"
echo
echo "$mems"
