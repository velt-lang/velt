#!/usr/bin/env bash
# TSX fortunes benchmark (issue #77): the TechEmpower fortunes page (13 rows) rendered 100 000
# times by hand-written template literals, by TSX precompiled through std/jsx, and by the same
# TSX through the generic lowering, plus a breakdown of the precompiled page. Prints a Markdown
# table: best wall-clock time over RUNS runs and the instructions retired by one run (macOS
# `time -l`; Linux `perf stat` when it is installed), each variant in its own process.
#
#   bench/jsx/run.sh [runs]
#   BASE=origin/main bench/jsx/run.sh   also builds the programs against that ref's std/ (same
#                                       compiler and runtime), for an A/B of std/jsx changes
#
# Needs: cargo, python3, git (for BASE).
set -euo pipefail
RUNS=${1:-7}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT="$ROOT/target/bench-jsx"
mkdir -p "$OUT"

echo "building velt (release) and the runtime..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
PYTHON=$(command -v python3 || command -v python)
VELT=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')/release/velt
VARIANTS=(hand precompiled generic rows-strings rows rows-render)

# measure <exe> <variant>: "<best ms> <instructions or ->" over RUNS runs.
measure() {
  "$PYTHON" - "$RUNS" "$@" <<'PY'
import re, shutil, subprocess, sys, time
runs, exe, variant = int(sys.argv[1]), sys.argv[2], sys.argv[3]
best = float("inf")
for _ in range(runs):
    t = time.perf_counter()
    subprocess.run([exe, variant], check=True, capture_output=True)
    best = min(best, time.perf_counter() - t)
instr = "-"
if sys.platform == "darwin":
    err = subprocess.run(["/usr/bin/time", "-l", exe, variant], capture_output=True, text=True).stderr
    m = re.search(r"(\d+)\s+instructions retired", err)
    instr = m.group(1) if m else "-"
elif shutil.which("perf"):
    err = subprocess.run(["perf", "stat", "-x,", "-e", "instructions", exe, variant],
                         capture_output=True, text=True).stderr
    m = re.search(r"^(\d+),", err, re.M)
    instr = m.group(1) if m else "-"
print(round(best * 1000), instr)
PY
}

# build <name> [std dir]: the benchmark against this checkout's std, or another one.
build() {
  local exe="$OUT/fortunes-$1"
  if [[ -n "${2:-}" ]]; then
    VELT_STD="$2" "$VELT" build --release "$HERE/main.vlt" -o "$exe" >&2
  else
    "$VELT" build --release "$HERE/main.vlt" -o "$exe" >&2
  fi
  "$exe" check >&2
  echo "$exe"
}

builds=("this checkout|$(build head)")
if [[ -n "${BASE:-}" ]]; then
  base_dir="$OUT/std-base"
  rm -rf "$base_dir" && mkdir -p "$base_dir"
  git -C "$ROOT" archive "$BASE" std | tar -x -C "$base_dir"
  builds=("std of $BASE|$(build base "$base_dir/std")" "${builds[@]}")
fi

echo "| std | variant | best ms | instructions |"
echo "|---|---|---:|---:|"
for b in "${builds[@]}"; do
  label=${b%%|*}
  exe=${b#*|}
  for v in "${VARIANTS[@]}"; do
    read -r ms instr < <(measure "$exe" "$v")
    echo "| $label | $v | $ms | $instr |"
  done
done
