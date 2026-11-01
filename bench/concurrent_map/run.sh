#!/usr/bin/env bash
# velt/collections (map.vlt, LLVM release) against bbtree's concurrent
# `BBTreeMap` (rust/: fat LTO, built for this CPU) on the same workloads. Checks that both print
# the same checksums, and prints a Markdown table of the best Mops/s over RUNS runs of each,
# interleaved so a busy machine affects them alike.
#
#   bench/concurrent_map/run.sh [runs]
#
# Needs: cargo, python3 on PATH; clang for the LLVM backend; network access the first time
# (bbtree is a git dependency). A run of both takes about a minute.
set -euo pipefail
RUNS=${1:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-concurrent-map"
mkdir -p "$OUT"

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
"$TARGET/release/velt" build --release --backend llvm "$HERE/map.vlt" -o "$OUT/map-velt"
(cd "$HERE/rust" && cargo build --release -q --target-dir "$OUT/rust")
cp "$OUT/rust/release/concurrent_map_bench" "$OUT/map-bbtree"

"$PYTHON" - "$RUNS" "$OUT" <<'PY'
import os, subprocess, sys
runs, out = int(sys.argv[1]), sys.argv[2]
exe = ".exe" if os.name == "nt" else ""
best, checks, order = {}, {}, []
for _ in range(runs):
    for lang in ("velt", "bbtree"):
        text = subprocess.run([f"{out}/map-{lang}{exe}"], check=True, capture_output=True, text=True).stdout
        for line in text.splitlines():
            name, threads, mops, check = line.split()
            key = (name, int(threads))
            if key not in order:
                order.append(key)
            best[lang, key] = max(best.get((lang, key), 0.0), float(mops))
            checks.setdefault(key, set()).add(check)
for key in order:
    if len(checks[key]) != 1:
        sys.exit(f"{key[0]} at {key[1]} threads: checksums differ: {sorted(checks[key])}")
print("| workload | threads | Velt Mops/s | bbtree Mops/s | Velt |")
print("|---|---:|---:|---:|---|")
for key in order:
    v, b = best["velt", key], best["bbtree", key]
    d = (v / b - 1) * 100
    print(f"| {key[0]} | {key[1]} | {v:.2f} | {b:.2f} | {abs(d):.0f}% {'faster' if d >= 0 else 'slower'} |")
print(f"\nBest of {runs} runs.")
PY
