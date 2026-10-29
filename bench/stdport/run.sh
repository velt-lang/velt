#!/usr/bin/env bash
# Standard-library components against the Rust crate that does the same job (Linux / macOS):
# each bench/stdport/<name>.vlt has a port in rust/src/bin/<name>.rs on the crate the runtime
# uses today (or, for what is already Velt code, the crate a Rust program would use). Builds
# both, checks that they print the same output, and prints a Markdown table of the best
# wall-clock time (ms) over RUNS runs, interleaved so a busy machine affects both columns alike.
# With valgrind on PATH it adds the instructions each one executes, which repeat to within 0.1%
# and are what a port is judged on (README.md).
#
#   bench/stdport/run.sh [runs] [only-benchmark]
#
# Builds into cargo's target directory (CARGO_TARGET_DIR when set).
# Needs: cargo, python3 on PATH; clang for the LLVM backend; valgrind for the instruction counts.
set -euo pipefail
RUNS=${1:-5}
ONLY=${2:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
# Cargo's target directory for this workspace: CARGO_TARGET_DIR, a cargo config, or <repo>/target.
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-stdport"
mkdir -p "$OUT"

echo "building velt (release), the runtime and the Rust ports..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
# The Rust ports are their own workspace: build them into a directory of their own, so a binary
# never has the name of one of velt's.
cargo build --release -q --manifest-path "$HERE/rust/Cargo.toml" --target-dir "$OUT/rust"
VELT="$TARGET/release/velt"
# The compressed inputs of the decompression rows, made by the Rust side's encoders.
export STDPORT_INPUTS="$OUT/inputs"
"$OUT/rust/release/gen_inputs" "$STDPORT_INPUTS"

names=()
for src in "$HERE"/*.vlt; do
  name=$(basename "$src" .vlt)
  [[ -n "$ONLY" && "$name" != "$ONLY" ]] && continue
  echo "$name..." >&2
  "$VELT" build --release "$src" -o "$OUT/$name-velt"
  names+=("$name")
done

"$PYTHON" - "$RUNS" "$OUT" "${names[@]}" <<'PY'
import os, re, shutil, subprocess, sys, time
runs, out, names = int(sys.argv[1]), sys.argv[2], sys.argv[3:]
env = dict(os.environ, VELT_THREADS="1")
cmds = {
    "velt": lambda n: [os.path.join(out, f"{n}-velt")],
    "rust": lambda n: [os.path.join(out, "rust", "release", n)],
}
best, outputs = {}, {}
for _ in range(runs):
    for n in names:
        for k, cmd in cmds.items():
            t = time.perf_counter()
            r = subprocess.run(cmd(n), check=True, capture_output=True, env=env).stdout
            dt = time.perf_counter() - t
            best[n, k] = min(best.get((n, k), float("inf")), dt)
            outputs.setdefault(n, {})[k] = r
for n in names:
    if outputs[n]["velt"] != outputs[n]["rust"]:
        sys.exit(f"{n}: Velt and Rust print different output:\n"
                 f"  velt: {outputs[n]['velt']!r}\n  rust: {outputs[n]['rust']!r}")

def instructions(cmd):
    grind = ["valgrind", "--tool=cachegrind", "--cache-sim=no", "--cachegrind-out-file=/dev/null"]
    r = subprocess.run(grind + cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=env, text=True)
    m = re.search(r"I\s+refs:\s+([\d,]+)", r.stderr)
    if r.returncode != 0 or not m:
        sys.exit(f"{cmd[0]}: cachegrind failed (exit {r.returncode}):\n{r.stderr[-2000:]}")
    return int(m.group(1).replace(",", ""))

counted = shutil.which("valgrind") is not None
head = "| benchmark | Velt ms | Rust ms | Velt / Rust |"
rule = "|---|---:|---:|---:|"
if counted:
    head += " Velt instr (M) | Rust instr (M) | Velt / Rust |"
    rule += "---:|---:|---:|"
print(head)
print(rule)
for n in names:
    v, r = (best[n, k] * 1000 for k in ("velt", "rust"))
    row = f"| {n} | {v:.0f} | {r:.0f} | {v / r:.2f} |"
    if counted:
        vi, ri = (instructions(cmds[k](n)) for k in ("velt", "rust"))
        row += f" {vi / 1e6:.0f} | {ri / 1e6:.0f} | {vi / ri:.2f} |"
    print(row)
print(f"\nBest of {runs} interleaved runs, wall-clock milliseconds including process start.")
if not counted:
    print("No valgrind on PATH: instruction counts left out.")
PY
