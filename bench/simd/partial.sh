#!/usr/bin/env bash
# partial.vlt at the array lengths of the last-elements question: the tail one element at a
# time against `loadPartial` / `storePartial`, both built from this tree with the LLVM backend:
#
#   bench/simd/partial.sh [RUNS]
#
# Prints a Markdown table of the instructions executed (cachegrind, `VELT_THREADS=1`) and the
# best wall time (ms) of RUNS interleaved runs (default 3). Builds into cargo's target
# directory (CARGO_TARGET_DIR when set). Needs: cargo, clang, valgrind, python3 (Linux).
set -euo pipefail
RUNS=${1:-3}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  python3 -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-simd"
mkdir -p "$OUT"
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
"$TARGET/release/velt" build --release --backend llvm "$HERE/partial.vlt" -o "$OUT/partial"

python3 - "$OUT/partial" "$RUNS" <<'PY'
import os, re, subprocess, sys, time
exe, runs = sys.argv[1], int(sys.argv[2])
env = dict(os.environ, VELT_THREADS="1")
def run(argv):
    return subprocess.run([exe] + argv, env=env, check=True, capture_output=True, text=True).stdout
def instructions(argv):
    log = exe + ".cg.log"
    subprocess.run(["valgrind", "--tool=cachegrind", "--cache-sim=no",
                    "--cachegrind-out-file=/dev/null", f"--log-file={log}", exe] + argv,
                   env=env, check=True, capture_output=True)
    return int(re.search(r"I\s+refs:\s+([\d,]+)", open(log).read()).group(1).replace(",", ""))
print("| length | loop instr | partial instr | partial/loop | loop ms | partial ms |")
print("|---:|---:|---:|---:|---:|---:|")
for n in (1, 3, 7, 15, 20, 31, 100):
    calls = "1000000"
    if run(["loop", str(n), calls]) != run(["partial", str(n), calls]):
        sys.exit(f"length {n}: the two variants print different output")
    ins = {m: instructions([m, str(n), calls]) for m in ("loop", "partial")}
    best = {"loop": 1e18, "partial": 1e18}
    for _ in range(runs):
        for m in best:
            t = time.perf_counter()
            run([m, str(n), calls])
            best[m] = min(best[m], (time.perf_counter() - t) * 1000)
    print(f"| {n} | {ins['loop']/1e6:,.1f} M | {ins['partial']/1e6:,.1f} M | "
          f"{ins['partial']/ins['loop']:.2f}x | {best['loop']:.0f} | {best['partial']:.0f} |",
          flush=True)
PY
