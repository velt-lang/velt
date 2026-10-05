#!/usr/bin/env bash
# TypeScript-style workloads: each bench/typical/*.vlt is also a TypeScript program Node runs
# as is (the harness appends the `main();` call), and rust/<name>.rs is the same algorithm in
# idiomatic Rust. Builds Velt (LLVM release), Rust (rustc -O) and runs Node (type stripping),
# checks that all three print the same output, and prints a Markdown table of the best
# wall-clock time (ms) over RUNS runs, interleaved so a busy machine affects all columns alike.
#
#   bench/typical/run.sh [runs] [only-benchmark]
#
# Needs: cargo, rustc, node (22.6 or newer), python3 on PATH; clang for the LLVM backend.
set -euo pipefail
RUNS=${1:-5}
ONLY=${2:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-typical"
mkdir -p "$OUT"

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
VELT="$TARGET/release/velt"

names=()
for src in "$HERE"/*.vlt; do
  name=$(basename "$src" .vlt)
  [[ -n "$ONLY" && "$name" != "$ONLY" ]] && continue
  echo "$name..." >&2
  "$VELT" build --release "$src" -o "$OUT/$name-velt"
  rustc -O --edition 2021 -o "$OUT/$name-rust" "$HERE/rust/$name.rs"
  { cat "$src"; echo; echo "main();"; } > "$OUT/$name.ts"
  names+=("$name")
done

"$PYTHON" - "$RUNS" "$OUT" "${names[@]}" <<'PY'
import os, subprocess, sys, time
runs, out, names = int(sys.argv[1]), sys.argv[2], sys.argv[3:]
exe = ".exe" if os.name == "nt" else ""
cmds = {
    "velt": lambda n: [os.path.join(out, f"{n}-velt{exe}")],
    "rust": lambda n: [os.path.join(out, f"{n}-rust{exe}")],
    "node": lambda n: ["node", "--experimental-strip-types", "--no-warnings", os.path.join(out, f"{n}.ts")],
}
best, outputs = {}, {}
for _ in range(runs):
    for n in names:
        for k, cmd in cmds.items():
            t = time.perf_counter()
            r = subprocess.run(cmd(n), check=True, capture_output=True).stdout
            dt = time.perf_counter() - t
            best[n, k] = min(best.get((n, k), float("inf")), dt)
            outputs.setdefault(n, {})[k] = r
for n in names:
    if len(set(outputs[n].values())) != 1:
        sys.exit(f"{n}: Velt, Rust and Node print different output")
print("| benchmark | Velt (LLVM release) | Rust -O | Node | Velt / Rust | Velt / Node |")
print("|---|---:|---:|---:|---:|---:|")
for n in names:
    v, r, j = (best[n, k] * 1000 for k in ("velt", "rust", "node"))
    print(f"| {n} | {v:.0f} | {r:.0f} | {j:.0f} | {v / r:.2f} | {v / j:.2f} |")
print(f"\nBest of {runs} interleaved runs, wall-clock milliseconds including process start.")
PY
