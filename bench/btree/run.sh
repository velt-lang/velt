#!/usr/bin/env bash
# Ordered maps: btree.vlt (Velt `BTreeMap`, LLVM release) against Rust's std `BTreeMap` and
# bbtree's (rust/: fat LTO, built for this CPU, as bbtree's own benchmark builds). "Velt, user
# code" is the same program with the map's sources copied out of std into its own modules, as a
# package would have them (crates/veltc/tests/btree_as_package.rs). Checks that all print the
# same checksums, and prints a Markdown table of the best time (ms) over RUNS runs,
# interleaved so a busy machine affects them alike.
#
#   bench/btree/run.sh [runs]
#
# Needs: cargo, python3 on PATH (and the network once, for bbtree); clang for the LLVM backend.
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
# The user-code copy: std's map as the program's own modules, imports made relative.
USER="$OUT/user"
rm -rf "$USER" && mkdir -p "$USER/btree"
sed 's#"velt/collections/btree/#"./btree/#g' "$ROOT/std/collections/btree_map.vlt" >"$USER/btree_map.vlt"
for f in "$ROOT"/std/collections/btree/*.vlt; do
  sed 's#"velt/collections/btree/#"./#g' "$f" >"$USER/btree/$(basename "$f")"
done
sed 's#"velt/collections"#"./btree_map"#' "$HERE/btree.vlt" >"$USER/main.vlt"
"$TARGET/release/velt" build --release --backend llvm "$USER/main.vlt" -o "$OUT/btree-velt-user"
(cd "$HERE/rust" && cargo build --release -q --target-dir "$OUT/rust")
cp "$OUT/rust/release/std" "$OUT/btree-std"
cp "$OUT/rust/release/bbtree" "$OUT/btree-bbtree"

"$PYTHON" - "$RUNS" "$OUT" <<'PY'
import os, subprocess, sys
runs, out = int(sys.argv[1]), sys.argv[2]
exe = ".exe" if os.name == "nt" else ""
best, checks, order = {}, {}, []
maps = ("velt", "velt-user", "bbtree", "std")
for _ in range(runs):
    for lang in maps:
        text = subprocess.run([f"{out}/btree-{lang}{exe}"], check=True, capture_output=True, text=True).stdout
        for line in text.splitlines():
            name, ms, check = line.split()
            if name not in order:
                order.append(name)
            best[lang, name] = min(best.get((lang, name), float("inf")), float(ms))
            checks[lang, name] = check
for name in order:
    if len({checks[m, name] for m in maps}) != 1:
        sys.exit(f"{name}: checksums differ: " + ", ".join(f"{m} {checks[m, name]}" for m in maps))
print("| workload | Velt BTreeMap | Velt, user code | bbtree | Rust std BTreeMap |")
print("|---|---:|---:|---:|---:|")
for name in order:
    print(f"| {name} | " + " | ".join(f"{best[m, name]:.1f}" for m in maps) + " |")
print(f"\nBest of {runs} runs, milliseconds (1M u64 keys, 200k string keys).")
PY
