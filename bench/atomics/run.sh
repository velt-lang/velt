#!/usr/bin/env bash
# Atomics: atomics.vlt (Velt `Atomic<T>` and velt/sync, LLVM release) against the same
# workloads in Rust (rust/: `std::sync::atomic`, fat LTO, built for this CPU). Checks that both
# print the same checksums (for the timing-dependent `latch_reads_4t`: no torn read, 0), and
# prints a Markdown table of the best time (ms) over RUNS runs, interleaved so a busy machine
# affects them alike.
#
#   bench/atomics/run.sh [runs]
#
# Needs: cargo, python3 on PATH; clang for the LLVM backend.
set -euo pipefail
RUNS=${1:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-atomics"
mkdir -p "$OUT"

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
"$TARGET/release/velt" build --release --backend llvm "$HERE/atomics.vlt" -o "$OUT/atomics-velt"
(cd "$HERE/rust" && cargo build --release -q --target-dir "$OUT/rust")
cp "$OUT/rust/release/atomics_bench" "$OUT/atomics-rust"

"$PYTHON" - "$RUNS" "$OUT" <<'PY'
import os, subprocess, sys
runs, out = int(sys.argv[1]), sys.argv[2]
exe = ".exe" if os.name == "nt" else ""
best, checks, order = {}, {}, []
langs = ("velt", "rust")
for _ in range(runs):
    for lang in langs:
        text = subprocess.run([f"{out}/atomics-{lang}{exe}"], check=True, capture_output=True, text=True).stdout
        for line in text.splitlines():
            name, ms, check = line.split()
            if name not in order:
                order.append(name)
            best[lang, name] = min(best.get((lang, name), float("inf")), float(ms))
            checks[lang, name] = check
for name in order:
    if len({checks[l, name] for l in langs}) != 1:
        sys.exit(f"{name}: checksums differ: " + ", ".join(f"{l} {checks[l, name]}" for l in langs))
print("| workload | Velt `Atomic<T>` | Rust `std::sync::atomic` | Velt / Rust |")
print("|---|---:|---:|---:|")
for name in order:
    v, r = best["velt", name], best["rust", name]
    print(f"| {name} | {v:.1f} | {r:.1f} | {v / r:.2f} |")
print(f"\nBest of {runs} runs, milliseconds.")
PY
