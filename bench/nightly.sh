#!/usr/bin/env bash
# Nightly benchmark guard (Linux): counts the instructions each benchmark executes and compares
# them with a baseline from an earlier run. Instructions, not time: a CI runner shares its
# machine, so wall time varies by tens of percent from run to run, while the instruction count of
# one program is the same every time (VELT_THREADS=1 keeps the async benchmarks on one worker).
#
#   bench/nightly.sh [--baseline FILE] [--out FILE] [--threshold PERCENT] [--velt PATH]
#
# Builds velt (release) unless --velt is given, builds the benchmarks below (LLVM, --release),
# runs each once natively (it must succeed; its time is shown, not compared) and once under
# valgrind's cachegrind for the count, plus the instructions of `velt check` over the whole
# standard library (bench/compile/all_std.vlt: parsing and type checking). Writes the counts to
# --out (default: $TARGET/bench-nightly/counts.tsv) and prints a Markdown table. With a baseline,
# it exits 1 when a benchmark executes more than PERCENT (default 3, the review rule) more
# instructions. Counts repeat to within 0.1%, so the band needs no room for noise.
# Needs: cargo, clang, valgrind, python3.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
PYTHON=$(command -v python3 || command -v python)
BASELINE=
OUT_FILE=
THRESHOLD=3
VELT=
while [ $# -gt 0 ]; do
  case "$1" in
    --baseline) BASELINE=$2; shift 2 ;;
    --out) OUT_FILE=$2; shift 2 ;;
    --threshold) THRESHOLD=$2; shift 2 ;;
    --velt) VELT=$2; shift 2 ;;
    *) echo "usage: bench/nightly.sh [--baseline FILE] [--out FILE] [--threshold PERCENT] [--velt PATH]" >&2; exit 2 ;;
  esac
done
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT="$TARGET/bench-nightly"
mkdir -p "$OUT"
OUT_FILE=${OUT_FILE:-$OUT/counts.tsv}

if [ -z "$VELT" ]; then
  echo "building velt (release) and the runtime..." >&2
  cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi

# The benchmarks: CPU-bound programs whose work does not depend on timing (bench/async/timers
# sleeps, the HTTP and database benchmarks need servers and load generators).
BENCHES=("$ROOT"/bench/*.vlt "$ROOT"/bench/iter/*.vlt "$ROOT"/bench/json/*.vlt
  "$ROOT"/bench/strings_utf16/*.vlt)
for name in await_chain await_deep hot_loop fanout_all fanout_all_throwing all_small_stored \
  spawn_many channel_pipeline; do
  BENCHES+=("$ROOT/bench/async/$name.vlt")
done

# count <name> <command...>: one line "<name>\t<instructions>\t<native ms>" on stdout.
count() {
  local name=$1; shift
  "$PYTHON" - "$name" "$@" <<'EOF'
import os, re, subprocess, sys, time
name, cmd = sys.argv[1], sys.argv[2:]
env = dict(os.environ, VELT_THREADS="1")
t = time.perf_counter()
subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, env=env)
ms = (time.perf_counter() - t) * 1000
grind = ["valgrind", "--tool=cachegrind", "--cache-sim=no", "--cachegrind-out-file=/dev/null"]
r = subprocess.run(grind + cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=env, text=True)
m = re.search(r"I\s+refs:\s+([\d,]+)", r.stderr)
if r.returncode != 0 or not m:
    sys.exit(f"{name}: cachegrind failed (exit {r.returncode}):\n{r.stderr[-2000:]}")
print(f"{name}\t{m.group(1).replace(',', '')}\t{ms:.0f}", flush=True)
EOF
}

: > "$OUT_FILE"
for src in "${BENCHES[@]}"; do
  rel=${src#"$ROOT/bench/"}
  name=${rel%.vlt}
  exe="$OUT/$(echo "$name" | tr '/' '_')"
  echo "$name" >&2
  "$VELT" build --release --backend llvm "$src" -o "$exe" >&2
  count "$name" "$exe" >> "$OUT_FILE"
done
echo "compile/check_all_std" >&2
count "compile/check_all_std" "$VELT" check "$ROOT/bench/compile/all_std.vlt" >> "$OUT_FILE"

"$PYTHON" - "$OUT_FILE" "$BASELINE" "$THRESHOLD" <<'EOF'
import sys
def load(path):
    rows = {}
    for line in open(path):
        name, instr, ms = line.rstrip("\n").split("\t")
        rows[name] = (int(instr), int(ms))
    return rows
new = load(sys.argv[1])
base = load(sys.argv[2]) if sys.argv[2] else {}
limit = float(sys.argv[3])
print("| benchmark | instructions (M) | baseline (M) | change | native ms |")
print("|---|---:|---:|---:|---:|")
worse = []
for name, (instr, ms) in new.items():
    if name in base:
        b = base[name][0]
        change = 100 * (instr - b) / b
        mark = ""
        if change > limit:
            worse.append(name)
            mark = " **regression**"
        print(f"| {name} | {instr / 1e6:.1f} | {b / 1e6:.1f} | {change:+.2f}%{mark} | {ms} |")
    else:
        print(f"| {name} | {instr / 1e6:.1f} | - | new | {ms} |")
print()
if not base:
    print("No baseline: these counts become the next run's baseline.")
elif worse:
    print(f"{len(worse)} benchmark(s) execute more than {limit:g}% more instructions than the "
          f"baseline: {', '.join(worse)}.")
    sys.exit(1)
else:
    print(f"Every benchmark is within {limit:g}% of the baseline (more instructions fail; fewer "
          "are improvements).")
EOF
