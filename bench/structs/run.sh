#!/usr/bin/env bash
# Struct benchmarks for the value-type design (#641): the same algorithm written with today's
# `struct` (<name>_struct.vlt), a `class` (<name>_class.vlt), by hand without objects
# (<name>_flat.vlt: parallel `number[]` arrays or packed keys, the code a guaranteed-inline
# value type should reach), where present today's `struct` updated field by field
# (<name>_mut.vlt: what a mutable struct allows today and the value type forbids) and the same
# with one shared value (<name>_shared.vlt: the struct type becomes a counted object), in Rust
# with a `#[derive(Clone, Copy)]` struct (rust/<name>.rs) and, for reference, the class and
# flat versions in Node (they are TypeScript as written; the harness appends `main();`).
#
#   bench/structs/run.sh [--velt PATH] [--rt-debug PATH] [--only NAME] [--no-node] [--out DIR] [--reuse]
#
# Checks that every version prints the same output, then prints Markdown tables of the
# instructions each Velt and Rust version executes (valgrind cachegrind, VELT_THREADS=1: the count
# repeats to within 0.1% from run to run, unlike wall time on a shared machine), of the best
# wall-clock time of 3 interleaved runs (the only column for Node, whose JIT valgrind cannot
# always run), and of the heap allocations: for
# Velt the `velt_rt_alloc` calls (VELT_RC_STATS=1 `blocks=` plus heap string buffers `alloc=`,
# counted by the debug runtime linked into the same optimized program), for Rust the blocks
# valgrind's DHAT sees.
# --velt: a release `velt` (default: build one); --rt-debug: the debug runtime library
# (default: `cargo build -p velt_rt`). Needs: cargo, rustc, clang, valgrind, python3; node for
# the Node column. Linux (valgrind). --reuse: keep the programs already built in --out.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
VELT=
RT_DEBUG=
ONLY=
NODE=1
OUT=
REUSE=
while [ $# -gt 0 ]; do
  case "$1" in
    --velt) VELT=$2; shift 2 ;;
    --rt-debug) RT_DEBUG=$2; shift 2 ;;
    --only) ONLY=$2; shift 2 ;;
    --no-node) NODE=; shift ;;
    --out) OUT=$2; shift 2 ;;
    --reuse) REUSE=1; shift ;;
    *) echo "usage: bench/structs/run.sh [--velt PATH] [--rt-debug PATH] [--only NAME] [--no-node] [--out DIR] [--reuse]" >&2; exit 2 ;;
  esac
done
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT=${OUT:-$TARGET/bench-structs}
mkdir -p "$OUT"
if [ -z "$VELT" ]; then
  echo "building velt (release) and the runtime..." >&2
  cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi
if [ -z "$RT_DEBUG" ]; then
  cargo build -q -p velt_rt --manifest-path "$ROOT/Cargo.toml"
  RT_DEBUG="$TARGET/debug/libvelt_rt.a"
fi

names=()
for src in "$HERE"/*_struct.vlt; do
  name=$(basename "$src" _struct.vlt)
  [[ -n "$ONLY" && "$name" != "$ONLY" ]] && continue
  echo "$name..." >&2
  kinds=(struct class flat)
  for extra in mut shared; do
    [ -f "$HERE/${name}_$extra.vlt" ] && kinds+=("$extra")
  done
  for kind in "${kinds[@]}"; do
    [[ -n "$REUSE" && -f "$OUT/$name-$kind" && -f "$OUT/$name-$kind-counting" ]] && continue
    "$VELT" build --release --backend llvm "$HERE/${name}_$kind.vlt" -o "$OUT/$name-$kind"
    VELT_RT_LIB="$RT_DEBUG" "$VELT" build --release --backend llvm "$HERE/${name}_$kind.vlt" \
      -o "$OUT/$name-$kind-counting"
  done
  rustc -O --edition 2021 -o "$OUT/$name-rust" "$HERE/rust/$name.rs"
  for kind in class flat; do
    { cat "$HERE/${name}_$kind.vlt"; echo; echo "main();"; } > "$OUT/$name-$kind.ts"
  done
  names+=("$name")
done

"$PYTHON" - "$OUT" "$NODE" "${names[@]}" <<'PY'
import os, re, subprocess, sys, time
out, node, names = sys.argv[1], sys.argv[2] == "1", sys.argv[3:]
env = dict(os.environ, VELT_THREADS="1")
VELT = ("struct", "mut", "shared", "class", "flat")
node_cmd = ["node", "--experimental-strip-types", "--no-warnings"]
def cmds(n):
    c = {k: [os.path.join(out, f"{n}-{k}")] for k in VELT + ("rust",)
         if os.path.exists(os.path.join(out, f"{n}-{k}"))}
    if node:
        for k in ("class", "flat"):
            c[f"node-{k}"] = node_cmd + [os.path.join(out, f"{n}-{k}.ts")]
    return c
def grind(cmd):
    r = subprocess.run(["valgrind", "--vgdb=no", "--tool=cachegrind", "--cache-sim=no",
                        "--cachegrind-out-file=/dev/null"] + cmd,
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=env, text=True)
    m = re.search(r"I\s+refs:\s+([\d,]+)", r.stderr)
    if r.returncode != 0 or not m:
        sys.exit(f"{cmd}: cachegrind failed (exit {r.returncode}):\n{r.stderr[-2000:]}")
    return int(m.group(1).replace(",", ""))
def velt_blocks(exe):
    r = subprocess.run([exe], capture_output=True, text=True, env=dict(env, VELT_RC_STATS="1"))
    # Heap blocks plus heap string buffers (`alloc=`): both are `velt_rt_alloc` calls.
    m = re.search(r"alloc=(\d+) .*blocks=(\d+)/", r.stderr)
    return int(m.group(1)) + int(m.group(2)) if m else None
def rust_blocks(exe):
    r = subprocess.run(["valgrind", "--vgdb=no", "--tool=dhat", "--dhat-out-file=/dev/null", exe],
                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    m = re.search(r"Total:\s+[\d,]+ bytes in ([\d,]+) blocks", r.stderr)
    return int(m.group(1).replace(",", "")) if m else None
rows = []
for n in names:
    c = cmds(n)
    outputs = {k: subprocess.run(v, check=True, capture_output=True, env=env).stdout for k, v in c.items()}
    if len(set(outputs.values())) != 1:
        sys.exit(f"{n}: the versions print different output: {outputs}")
    ir = {}
    for k, v in c.items():
        if k.startswith("node"):
            continue
        print(f"{n} {k}...", file=sys.stderr, flush=True)
        ir[k] = grind(v)
    wall = {}
    for _ in range(3):
        for k, v in c.items():
            t = time.perf_counter()
            subprocess.run(v, check=True, stdout=subprocess.DEVNULL, env=env)
            wall[k] = min(wall.get(k, float("inf")), (time.perf_counter() - t) * 1000)
    blocks = {k: velt_blocks(os.path.join(out, f"{n}-{k}-counting")) for k in VELT if k in c}
    blocks["rust"] = rust_blocks(os.path.join(out, f"{n}-rust"))
    rows.append((n, ir, blocks, wall))
M = lambda x: f"{x / 1e6:,.0f}"
x = lambda a, b: f"{a / b:.2f}"
cols = list(VELT) + ["rust"]
wcols = cols + (["node-class", "node-flat"] if node else [])
print("Instructions executed (millions; cachegrind, VELT_THREADS=1):\n")
print("| workload | " + " | ".join(cols) + " | struct / flat | class / flat | flat / rust | struct / rust |")
print("|---|" + "---:|" * (len(cols) + 4))
for n, ir, _, _ in rows:
    print(f"| {n} | " + " | ".join(M(ir[k]) if k in ir else "–" for k in cols)
          + f" | {x(ir['struct'], ir['flat'])} | {x(ir['class'], ir['flat'])} | {x(ir['flat'], ir['rust'])} | {x(ir['struct'], ir['rust'])} |")
print("\nBest wall-clock time of 3 interleaved runs (ms, including process start):\n")
print("| workload | " + " | ".join(wcols) + " |")
print("|---|" + "---:|" * len(wcols))
for n, _, _, w in rows:
    print(f"| {n} | " + " | ".join(f"{w[k]:.0f}" if k in w else "–" for k in wcols) + " |")
print("\nHeap blocks allocated (Velt: `velt_rt_alloc`, VELT_RC_STATS; Rust: DHAT):\n")
print("| workload | " + " | ".join(cols) + " |")
print("|---|" + "---:|" * len(cols))
for n, _, b, _ in rows:
    print(f"| {n} | " + " | ".join(f"{b[k]:,}" if b.get(k) is not None else "–"
                                   for k in cols) + " |")
PY
