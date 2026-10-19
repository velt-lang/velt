#!/usr/bin/env bash
# SIMD kernels: kernels.vlt (Velt, LLVM release) against kernels.rs (rustc -O), each kernel as
# a plain loop and with SIMD vectors (Velt: `u8x16` / `f32x4`; Rust: `core::arch` intrinsics for
# the host, NEON or SSE2). Checks that both print the same checksums, and prints a Markdown
# table of the best time (ms) over RUNS runs, interleaved so a busy machine affects both alike.
#
#   bench/simd/run.sh [runs]
#
# Needs: cargo, rustc, python3 on PATH; clang for the LLVM backend.
set -euo pipefail
RUNS=${1:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-simd"
mkdir -p "$OUT"

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
"$TARGET/release/velt" build --release --backend llvm "$HERE/kernels.vlt" -o "$OUT/kernels-velt"
rustc -O --edition 2021 -o "$OUT/kernels-rust" "$HERE/kernels.rs"

"$PYTHON" - "$RUNS" "$OUT" <<'PY'
import os, subprocess, sys
runs, out = int(sys.argv[1]), sys.argv[2]
exe = ".exe" if os.name == "nt" else ""
best, checks = {}, {}
for _ in range(runs):
    for lang in ("velt", "rust"):
        text = subprocess.run([f"{out}/kernels-{lang}{exe}"], check=True, capture_output=True, text=True).stdout
        for line in text.splitlines():
            name, ms, check = line.split()
            best[lang, name] = min(best.get((lang, name), 1 << 60), int(ms))
            checks[lang, name] = check
kernels = sorted({name.rsplit("_", 1)[0] for _, name in best}, key=lambda k: list(best).index(("velt", next(n for l, n in best if n.startswith(k)))))
for (lang, name), check in checks.items():
    if checks["rust", name] != check:
        sys.exit(f"{name}: Velt prints {checks['velt', name]}, Rust {checks['rust', name]}")
print("| kernel | Velt loop | Velt SIMD | Rust loop | Rust intrinsics |")
print("|---|---:|---:|---:|---:|")
cell = lambda lang, name: str(best[lang, name]) if (lang, name) in best else "–"
for k in kernels:
    print(f"| {k} | {cell('velt', k + '_loop')} | {cell('velt', k + '_simd')} | {cell('rust', k + '_loop')} | {cell('rust', k + '_simd')} |")
print(f"\nBest of {runs} runs, milliseconds per kernel (1.6 GB of data each).")
PY
