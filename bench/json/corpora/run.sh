#!/usr/bin/env bash
# The standard JSON corpora (fetch.sh) against Rust's serde_json, sonic-rs and simd-json and
# Node: `value` parses a corpus into a dynamic value ROUNDS times (Velt: JSON.parseValue);
# `write` parses it once and writes it back ROUNDS times. Prints a Markdown table of the best of
# RUNS runs of each whole process (start-up and the file read included, no separate warm-up).
#
#   bench/json/corpora/run.sh [runs] [rounds]
#
# Checks: for `value`, every program prints the corpus's top-level member count; for `write`,
# each prints the last text it wrote, and Velt's must equal Node's JSON.stringify byte for byte,
# while the Rust rows (which format numbers their own way, e.g. `1.0`, and simd-json does not
# keep object order) must parse back to the same value as Node's.
#
# MEASURE=time (default): best wall-clock time in ms and MB/s (input bytes x rounds / time).
# MEASURE=instructions (macOS): the fewest instructions retired (`/usr/bin/time -l`), in
# billions, and per input byte (x rounds); steadier than time on a loaded machine.
# VELT=<path to a release velt> skips building the compiler (set VELT_STD and VELT_RT_LIB to
# match); otherwise velt and velt_rt are built (release) in the workspace's target directory.
# The first run downloads the corpora (about 4.4 MB, from GitHub; fetch.sh); later runs only
# check their SHA-256. There is no Windows version.
# Needs: cargo, clang (the LLVM backend's linker driver), python3, node, curl.
set -euo pipefail
RUNS=${1:-5}
ROUNDS=${2:-50}
MEASURE=${MEASURE:-time}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)

missing=()
for tool in cargo clang python3 node curl; do
  command -v "$tool" > /dev/null || missing+=("$tool")
done
if ((${#missing[@]})); then
  echo "run.sh: missing ${missing[*]} (needs cargo, clang, python3, node, curl)" >&2
  exit 1
fi
case $MEASURE in
  time) ;;
  instructions)
    if [[ $(uname) != Darwin ]]; then
      echo "run.sh: MEASURE=instructions needs macOS (/usr/bin/time -l)" >&2
      exit 1
    fi
    ;;
  *)
    echo "run.sh: MEASURE must be time or instructions, not $MEASURE" >&2
    exit 1
    ;;
esac

TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  python3 -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
OUT="$TARGET/bench-json-corpora"
mkdir -p "$OUT"
"$HERE/fetch.sh"
if [[ -z ${VELT:-} ]]; then
  echo "building velt (release)..." >&2
  cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi
echo "building the Rust benchmarks..." >&2
cargo build --release -q --manifest-path "$HERE/rust/Cargo.toml" --target-dir "$TARGET"
for op in value write; do
  "$VELT" build --release "$HERE/$op.vlt" -o "$OUT/$op"
done

# best <check> <command...>: the best measurement over RUNS runs. <check> is `-` (store the
# output as the reference), `same` (equal to the reference byte for byte) or `json` (parses to
# the same value as the reference).
best() {
  local check=$1
  shift
  python3 - "$RUNS" "$MEASURE" "$check" "$OUT/expected" "$@" << 'PY'
import json, re, subprocess, sys, time
runs, measure, check, ref, cmd = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5:]
best = float("inf")
for _ in range(runs):
    if measure == "instructions":
        p = subprocess.run(["/usr/bin/time", "-l"] + cmd, capture_output=True)
        m = re.search(rb"(\d+)\s+instructions retired", p.stderr)
        if p.returncode != 0 or not m:
            sys.exit(f"{' '.join(cmd)}: failed: {p.stderr[-400:]!r}")
        best = min(best, int(m.group(1)))
    else:
        t = time.perf_counter()
        p = subprocess.run(cmd, capture_output=True)
        best = min(best, time.perf_counter() - t)
        if p.returncode != 0:
            sys.exit(f"{' '.join(cmd)}: failed: {p.stderr[-400:]!r}")
out = p.stdout
if check == "-":
    open(ref, "wb").write(out)
else:
    expected = open(ref, "rb").read()
    same = out == expected if check == "same" else json.loads(out) == json.loads(expected)
    if not same:
        sys.exit(f"{' '.join(cmd)}: output differs from Node's ({len(out)} vs {len(expected)} bytes)")
print(best)
PY
}

# cell <measurement> <input bytes>
cell() {
  python3 -c '
import sys
m, size, rounds, measure = float(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
if measure == "instructions":
    print(f"{m / 1e9:.2f} G ({m / (size * rounds):.0f}/B)")
else:
    print(f"{m * 1000:.0f} ({size * rounds / max(m, 1e-6) / 1e6:.0f} MB/s)")
' "$1" "$2" "$ROUNDS" "$MEASURE"
}

if [[ $MEASURE == time ]]; then
  echo "best of $RUNS, $ROUNDS rounds: ms (MB/s)" >&2
else
  echo "fewest of $RUNS, $ROUNDS rounds: instructions retired (per input byte)" >&2
fi
echo "| corpus | op | Velt | serde_json | sonic-rs | simd-json | Node |"
echo "|---|---|---:|---:|---:|---:|---:|"
for name in canada citm_catalog twitter; do
  f="$HERE/data/$name.json"
  size=$(wc -c < "$f")
  for op in value write; do
    if [[ $op == value ]]; then rust=same; else rust=json; fi
    node=$(best - node "$HERE/node/$op.js" "$f" "$ROUNDS")
    velt=$(best same "$OUT/$op" "$f" "$ROUNDS")
    serde=$(best $rust "$TARGET/release/${op}_serde" "$f" "$ROUNDS")
    sonic=$(best $rust "$TARGET/release/${op}_sonic" "$f" "$ROUNDS")
    simd=$(best $rust "$TARGET/release/${op}_simd" "$f" "$ROUNDS")
    echo "| $name | $op | $(cell "$velt" "$size") | $(cell "$serde" "$size") | $(cell "$sonic" "$size") | $(cell "$simd" "$size") | $(cell "$node" "$size") |"
  done
done
