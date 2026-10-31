#!/usr/bin/env bash
# Positioned file I/O: pread.vlt (velt/fs's fd API, Velt over system functions, LLVM release)
# against Node (pread.js: fs.readSync with a position) and Rust (rust/: FileExt::read_at, fat
# LTO). Workloads: random 4 KiB reads of a 64 MiB file (in the OS cache after a warm-up),
# sequential 64 KiB reads of it, and 100-byte writes each followed by fsync. Checks that all
# print the same checksums, and prints a Markdown table of the best time (ms) of RUNS runs,
# interleaved so a busy machine affects them alike.
#
#   bench/fs/pread/run.sh [runs]
#
# Needs: cargo, node, python3 on PATH; clang for the LLVM backend. macOS or Linux.
set -euo pipefail
RUNS=${1:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-pread"
mkdir -p "$OUT"

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
"$TARGET/release/velt" build --release --backend llvm "$HERE/pread.vlt" -o "$OUT/pread-velt"
(cd "$HERE/rust" && cargo build --release -q --target-dir "$OUT/rust")
cp "$OUT/rust/release/pread_bench" "$OUT/pread-rust"
DATA="$OUT/data.bin"
[ -f "$DATA" ] || "$PYTHON" -c "import os, sys; open(sys.argv[1], 'wb').write(os.urandom(64 << 20))" "$DATA"

"$PYTHON" - "$RUNS" "$OUT" "$HERE/pread.js" "$DATA" <<'PY'
import subprocess, sys
runs, out, js, data = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4]
cmds = {
    "velt": [f"{out}/pread-velt", data],
    "rust": [f"{out}/pread-rust", data],
    "node": ["node", js, data],
}
for cmd in cmds.values():  # warm-up: the file in the OS cache
    subprocess.run(cmd, check=True, capture_output=True)
best, checks, order = {}, {}, []
for _ in range(runs):
    for lang, cmd in cmds.items():
        text = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout
        for line in text.splitlines():
            name, ms, check = line.split()
            if name not in order:
                order.append(name)
            best[lang, name] = min(best.get((lang, name), float("inf")), float(ms))
            checks.setdefault(name, set()).add(check)
for name, cs in checks.items():
    if len(cs) != 1:
        sys.exit(f"{name}: the checksums differ: {cs}")
print("| workload | Velt (ms) | Rust (ms) | Node (ms) | Velt / Rust | Node / Velt |")
print("|---|---:|---:|---:|---:|---:|")
for name in order:
    v, r, n = best["velt", name], best["rust", name], best["node", name]
    print(f"| {name} | {v:.1f} | {r:.1f} | {n:.1f} | {v / r:.2f} | {n / v:.2f} |")
PY
