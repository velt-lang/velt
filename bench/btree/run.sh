#!/usr/bin/env bash
# Ordered maps: btree.vlt (Velt `BTreeMap`, LLVM release) against btree.rs (Rust's std
# `BTreeMap`, rustc -O). Checks that both print the same checksums, and prints a Markdown table
# of the best time (ms) over RUNS runs, interleaved so a busy machine affects both alike.
#
#   bench/btree/run.sh [runs]
#
# Needs: cargo, rustc, python3 on PATH; clang for the LLVM backend.
set -euo pipefail
RUNS=${1:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-btree"
mkdir -p "$OUT"

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
"$TARGET/release/velt" build --release --backend llvm "$HERE/btree.vlt" -o "$OUT/btree-velt"
rustc -O --edition 2021 -o "$OUT/btree-rust" "$HERE/btree.rs"

"$PYTHON" - "$RUNS" "$OUT" <<'PY'
import os, subprocess, sys
runs, out = int(sys.argv[1]), sys.argv[2]
exe = ".exe" if os.name == "nt" else ""
best, checks, order = {}, {}, []
for _ in range(runs):
    for lang in ("velt", "rust"):
        text = subprocess.run([f"{out}/btree-{lang}{exe}"], check=True, capture_output=True, text=True).stdout
        for line in text.splitlines():
            name, ms, check = line.split()
            if name not in order:
                order.append(name)
            best[lang, name] = min(best.get((lang, name), 1 << 60), int(ms))
            checks[lang, name] = check
for name in order:
    if checks["velt", name] != checks["rust", name]:
        sys.exit(f"{name}: Velt prints {checks['velt', name]}, Rust {checks['rust', name]}")
print("| workload | Velt BTreeMap | Rust std BTreeMap |")
print("|---|---:|---:|")
for name in order:
    print(f"| {name} | {best['velt', name]} | {best['rust', name]} |")
print(f"\nBest of {runs} runs, milliseconds (1M u64 keys, 200k string keys).")
PY
