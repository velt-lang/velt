#!/usr/bin/env bash
# The benchmark showcase (Linux): Velt against Rust, Node and Bun on the existing suites under
# bench/, every category in one run. Method and fairness rules: bench/showcase/README.md.
#
#   bench/showcase/run.sh [--smoke] [--force] [--runs N] [--out DIR] [--categories a,b,...]
#                         [--only name,...] [--velt PATH] [--http-duration 10s] [--http-conns 256]
#
#   --smoke        tiny sizes and one run of everything: checks that it all works, nothing more
#                  (results go to the build directory, not bench/showcase/results)
#   --force        run even when the 1-minute load average is over 2 (the numbers will be noisy)
#   --runs N       timed runs per implementation (default 5; start-up runs 4 × N)
#   --out DIR      where results.json and the tables go (default bench/showcase/results/<date>)
#   --categories   a subset of: startup cpu core benchmarks-game regex async json tsx sqlite
#                  compile http websocket
#   --only         run only the benchmarks whose name contains one of these strings
#   --velt PATH    use this release velt instead of building one (set VELT_STD and VELT_RT_LIB to match)
#
# Needs: cargo, rustc, clang, python3, node + npm, bun (~/.bun/bin is searched too), oha (for
# http), taskset, curl (the JSON corpora are downloaded once), git; network access the first time
# (crates, npm packages). Builds into $CARGO_TARGET_DIR/showcase (or <repo>/target/showcase).
# Stops only the processes it started.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
SMOKE=0 FORCE=0 RUNS=5 OUT="" CATS="" ONLY="" VELT="" HTTP_DURATION=10s HTTP_CONNS=256
while [[ $# -gt 0 ]]; do
  case $1 in
    --smoke) SMOKE=1 ;;
    --force) FORCE=1 ;;
    --runs) RUNS=$2; shift ;;
    --out) OUT=$2; shift ;;
    --categories) CATS=$2; shift ;;
    --only) ONLY=$2; shift ;;
    --velt) VELT=$2; shift ;;
    --http-duration) HTTP_DURATION=$2; shift ;;
    --http-conns) HTTP_CONNS=$2; shift ;;
    -h|--help) sed -n '2,23p' "$0"; exit 0 ;;
    *) echo "run.sh: unknown argument $1 (see --help)" >&2; exit 2 ;;
  esac
  shift
done
if [[ $(uname) != Linux ]]; then
  echo "run.sh: Linux only (it reads /proc and pins servers with taskset)" >&2
  exit 2
fi
PYTHON=$(command -v python3)

# 1. A quiet machine: a busy one skews wall-clock times by 2-4x.
load1=$(cut -d' ' -f1 /proc/loadavg)
if "$PYTHON" -c "import sys; sys.exit(0 if float(sys.argv[1]) > 2 else 1)" "$load1"; then
  if [[ $FORCE -eq 1 || $SMOKE -eq 1 ]]; then
    echo "run.sh: load average $load1 is over 2; going on (--force or --smoke), the times will be noisy" >&2
  else
    echo "run.sh: load average $load1 is over 2: the machine is busy. Wait until it is quiet, or pass --force." >&2
    exit 1
  fi
fi

# 2. The CPU frequency governor: pinned to performance when this user may (restored at exit),
# otherwise recorded as it is.
GOV_FILES=(/sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor)
OLD_GOV=$(cat "${GOV_FILES[0]}" 2>/dev/null || echo unknown)
GOVERNOR="$OLD_GOV (not changed: no permission)"
if [[ ! -f ${GOV_FILES[0]} ]]; then
  GOVERNOR="unknown (no cpufreq)"
elif [[ $OLD_GOV == performance ]]; then
  GOVERNOR=performance
elif [[ -w ${GOV_FILES[0]} ]]; then
  for f in "${GOV_FILES[@]}"; do echo performance > "$f"; done
  GOVERNOR="performance (pinned for the run; was $OLD_GOV)"
  trap 'for f in "${GOV_FILES[@]}"; do echo "$OLD_GOV" > "$f"; done' EXIT
elif sudo -n true 2>/dev/null; then
  for f in "${GOV_FILES[@]}"; do echo performance | sudo -n tee "$f" > /dev/null; done
  GOVERNOR="performance (pinned for the run; was $OLD_GOV)"
  trap 'for f in "${GOV_FILES[@]}"; do echo "$OLD_GOV" | sudo -n tee "$f" > /dev/null; done' EXIT
fi
export SHOWCASE_GOVERNOR=$GOVERNOR
echo "governor: $GOVERNOR" >&2

# 3. Velt in release, with this checkout's std.
TARGET=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')
if [[ -z $VELT ]]; then
  echo "building velt (release) and the runtimes..." >&2
  cargo build --release -q -p veltc -p velt_rt -p velt_rt_shared --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi
export VELT_STD=${VELT_STD:-$ROOT/std}
BUILD="$TARGET/showcase"
DATE=$(date +%Y-%m-%d)
if [[ -z $OUT ]]; then
  if [[ $SMOKE -eq 1 ]]; then OUT="$BUILD/smoke-results/$DATE"; else OUT="$HERE/results/$DATE"; fi
fi
mkdir -p "$BUILD" "$OUT"

args=(--out "$OUT" --build-dir "$BUILD" --runs "$RUNS" --velt "$VELT"
  --http-duration "$HTTP_DURATION" --http-conns "$HTTP_CONNS")
[[ $SMOKE -eq 1 ]] && args+=(--smoke)
[[ -n $CATS ]] && args+=(--categories "$CATS")
[[ -n $ONLY ]] && args+=(--only "$ONLY")
"$PYTHON" "$HERE/showcase.py" "${args[@]}"
