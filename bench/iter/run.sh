#!/usr/bin/env bash
# Iteration benchmark harness (Linux / macOS): same as run.ps1. Builds every bench/iter/*.vlt
# (LLVM release), checks that each prints what its Node version (node/<name>.js) prints, and
# prints a Markdown table of the best wall-clock time (ms) over RUNS interleaved rounds, with
# each program's time relative to hand_loop.
#
#   bench/iter/run.sh [runs]
#
# The programs compute the same sum over 0..N: hand_loop.vlt with a while loop, gen_loop.vlt with
# `for...of` over a generator call (the state embedded in the loop), gen_value.vlt with a
# generator passed as an `Iterable<i64>`, iterable_class.vlt with an iterator class. The async
# pair sums 20 × 3M values awaiting an async call per value: async_hand.vlt in a while loop,
# async_gen.vlt with `for await` over an async generator call; they are compared with
# async_hand. The array pair sums a 3M-element array 200 times: array_loop.vlt with `for...of`
# over the array, array_iterable.vlt through an `Iterable<i64>` parameter; they are compared with
# array_loop. The spread pair builds 20 arrays of 5M values and sums them: spread_gen.vlt with
# `[...range(n)]`, spread_hand.vlt pushing in a while loop; they are compared with spread_hand.
# Node runs once per program (its generators take tens of seconds here).
# Needs: cargo, node, python3 on PATH; clang for the LLVM backend.
set -euo pipefail
RUNS=${1:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-iter"
mkdir -p "$OUT"

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
VELT="$TARGET/release/velt"

names=()
for src in "$HERE"/*.vlt; do
  name=$(basename "$src" .vlt)
  names+=("$name")
  "$VELT" build --release --backend llvm "$src" -o "$OUT/$name"
done

"$PYTHON" - "$RUNS" "$OUT" "$HERE/node" "${names[@]}" <<'EOF'
import subprocess, sys, time
runs, out, node_dir, names = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4:]

def timed(cmd):
    t = time.perf_counter()
    res = subprocess.run(cmd, check=True, capture_output=True).stdout
    return time.perf_counter() - t, res

best = {n: float("inf") for n in names}
outputs = {}
for _ in range(runs):
    for n in names:  # interleaved: a slow moment hits every program alike
        t, res = timed([f"{out}/{n}"])
        best[n] = min(best[n], t)
        outputs[n] = res
node = {}
for n in names:
    t, res = timed(["node", f"{node_dir}/{n}.js"])
    if res != outputs[n]:
        sys.exit(f"{n}: Velt and Node print different results")
    node[n] = t
print("| benchmark | Velt LLVM release (ms) | vs baseline | Node (ms) |")
print("|---|---|---|---|")
for n in names:
    group = n.split("_")[0]
    base = best.get(
        {"async": "async_hand", "array": "array_loop", "spread": "spread_hand"}.get(group, "hand_loop")
    )
    rel = f"{best[n] / base:.2f}x" if base else "-"
    print(f"| {n} | {round(best[n] * 1000)} | {rel} | {round(node[n] * 1000)} |")
EOF
