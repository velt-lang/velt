#!/usr/bin/env bash
# Nightly benchmark guard (Linux): counts the instructions each benchmark executes and compares
# them with a baseline from an earlier run. Instructions, not time: a CI runner shares its
# machine, so wall time varies by tens of percent from run to run, while the instruction count of
# one program is the same every time (VELT_THREADS=1 and VELT_RESCUE=0 keep the async benchmarks
# on one worker).
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
# compile/check_all_std is compared per code line of std/ (its row has a fourth column, the
# non-blank, non-comment lines of std/**/*.vlt): the standard library grows with almost every
# feature, and checking more of it is not a slowdown, while checking each line more slowly is. A
# baseline row without the lines is compared in total.
# Benchmarks with a Node version (bench/node/<name>.js, bench/json/node/<name>.js) also run under
# `node`, best of 3 interleaved, and a second table marks each one where Velt takes longer than
# Node (wall time, so it is reported, not failed; the times go to <out>.node.tsv).
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
  "$ROOT"/bench/strings_utf16/*.vlt "$ROOT"/bench/stdport/*.vlt "$ROOT"/bench/spread_order/*.vlt)
# NIGHTLY_EXTRA_DIRS="typical …": also the benchmarks in bench/<dir>/*.vlt (for an A/B of a change).
for dir in ${NIGHTLY_EXTRA_DIRS:-}; do
  BENCHES+=("$ROOT/bench/$dir"/*.vlt)
done
for name in await_chain await_deep hot_loop fanout_all fanout_all_throwing all_small_stored \
  spawn_many channel_pipeline timers_fire top_level_await timer_await timer_await_top \
  promise_identity_loop; do
  BENCHES+=("$ROOT/bench/async/$name.vlt")
done

# count <name> <command...>: one line "<name>\t<instructions>\t<native ms>" on stdout.
count() {
  local name=$1; shift
  "$PYTHON" - "$name" "$@" <<'EOF'
import os, re, subprocess, sys, time
name, cmd = sys.argv[1], sys.argv[2:]
env = dict(os.environ, VELT_THREADS="1")
# The async benchmarks spawn in long polls: without this, the runtime's rescue of a stalled
# worker starts spares and moves tasks whenever a thread waits for its turn, which under valgrind
# (one thread at a time) depends on timing (#1673).
if name.startswith("async/"):
    env["VELT_RESCUE"] = "0"
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

# against_node <name> <velt exe> <node script>: one line "<name>\t<velt ms>\t<node ms>", the best
# of 3 interleaved runs of each (wall time, so only a guide: the job doesn't fail on it). When
# either program fails, it prints a note on stderr and no line.
against_node() {
  "$PYTHON" - "$@" <<'EOF'
import subprocess, sys, time
name, exe, script = sys.argv[1:]
best = {}
for _ in range(3):
    for k, cmd in (("velt", [exe]), ("node", ["node", script])):
        t = time.perf_counter()
        try:
            r = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
            failed = f"exited {r.returncode}:\n{r.stderr[-500:]}" if r.returncode != 0 else None
        except OSError as e:
            failed = f"did not start: {e}"
        if failed:
            print(f"{name}: {k} {failed}\n(not compared with Node)", file=sys.stderr)
            sys.exit(0)
        best[k] = min(best.get(k, float("inf")), time.perf_counter() - t)
print(f"{name}\t{best['velt'] * 1000:.0f}\t{best['node'] * 1000:.0f}", flush=True)
EOF
}

NODE_FILE="${OUT_FILE%.tsv}.node.tsv"
: > "$OUT_FILE"
: > "$NODE_FILE"
for src in "${BENCHES[@]}"; do
  rel=${src#"$ROOT/bench/"}
  name=${rel%.vlt}
  exe="$OUT/$(echo "$name" | tr '/' '_')"
  echo "$name" >&2
  "$VELT" build --release --backend llvm "$src" -o "$exe" >&2
  count "$name" "$exe" >> "$OUT_FILE"
  # The Node version of the same program, when there is one (bench/node/, bench/json/node/).
  js="$(dirname "$src")/node/$(basename "$name").js"
  if [ -f "$js" ] && command -v node > /dev/null; then
    against_node "$name" "$exe" "$js" >> "$NODE_FILE"
  fi
done
echo "compile/check_all_std" >&2
# Code lines only: documentation added to std/ would otherwise lower the cost per line and hide a
# slowdown of the same size. Not blank lines, `//` lines, or the lines of a comment that starts a
# line (`/** … */`).
std_lines=$(find "$ROOT/std" -name '*.vlt' -exec cat {} + | awk '
  block { if (/\*\//) block = 0; next }
  /^[ \t]*(\/\/|$)/ { next }
  /^[ \t]*\/\*/ { if (!/\*\//) block = 1; next }
  { n++ }
  END { print n + 0 }')
row=$(count "compile/check_all_std" "$VELT" check "$ROOT/bench/compile/all_std.vlt")
printf '%s\t%s\n' "$row" "$std_lines" >> "$OUT_FILE"

"$PYTHON" - "$OUT_FILE" "$BASELINE" "$THRESHOLD" "$NODE_FILE" <<'EOF'
import sys
def load(path):
    rows = {}
    for line in open(path):
        name, instr, ms, *lines = line.rstrip("\n").split("\t")
        rows[name] = (int(instr), int(ms), int(lines[0]) if lines else None)
    return rows
new = load(sys.argv[1])
base = load(sys.argv[2]) if sys.argv[2] else {}
limit = float(sys.argv[3])
print("| benchmark | instructions (M) | baseline (M) | change | native ms |")
print("|---|---:|---:|---:|---:|")
worse = []
for name, (instr, ms, lines) in new.items():
    if name in base:
        b, _, b_lines = base[name]
        change = 100 * (instr - b) / b
        label = name
        if lines and b_lines:
            # Per code line of std/: the lines are in the label, the change is per line.
            change = 100 * (instr / lines - b / b_lines) / (b / b_lines)
            label = f"{name} (per code line of std: {b_lines} -> {lines} lines)"
        mark = ""
        if change > limit:
            worse.append(name)
            mark = " **regression**"
        print(f"| {label} | {instr / 1e6:.1f} | {b / 1e6:.1f} | {change:+.2f}%{mark} | {ms} |")
    else:
        print(f"| {name} | {instr / 1e6:.1f} | - | new | {ms} |")
print()
# Against Node: wall time of the benchmarks that have a Node version. A row where Velt takes
# longer is marked; it doesn't fail the job (a shared runner's times vary by tens of percent).
# (A benchmark whose Velt or Node run failed has no row and is left out.)
against = [r for r in (l.rstrip("\n").split("\t") for l in open(sys.argv[4])) if len(r) == 3]
if against:
    print("| benchmark (wall ms, best of 3) | Velt | Node | Velt / Node |")
    print("|---|---:|---:|---:|")
    slower = []
    for name, v, n in against:
        v, n = int(v), int(n)
        ratio = v / n if n else float("inf")
        mark = ""
        if ratio > 1:
            slower.append(name)
            mark = " **slower than Node**"
        print(f"| {name} | {v} | {n} | {ratio:.2f}{mark} |")
    print()
    if slower:
        print(f"Velt takes longer than Node on {len(slower)} benchmark(s): {', '.join(slower)}.")
    else:
        print("Velt is faster than Node on every benchmark with a Node version.")
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
