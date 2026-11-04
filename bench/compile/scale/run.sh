#!/usr/bin/env bash
# Compile-speed scale benchmark (Linux / macOS): generates a Velt program of LINES lines (gen.py:
# modules of classes, subclasses, generic instances, strings, arrays) and times `velt check`,
# `velt build` (debug, Cranelift) and `velt build --release`, each with a fresh empty VELT_HOME
# and the build caches off, so nothing is reused between runs. Prints CPU (user + sys) and wall
# seconds, peak memory, the per-stage times of `-v`, and the cost per 100k lines.
#
# With GO set to a `go` binary (or `go` on PATH), it builds the same program in Go too: std
# warmed in a fresh GOCACHE first (Go ships std compiled), then the generated packages built
# from scratch, which is the comparison for Velt compiling the program's code.
#
#   bench/compile/scale/run.sh [lines = 100000] [path to velt] [out dir]
#
# Without a velt path it builds velt (release) first. Needs python3.
set -euo pipefail
LINES=${1:-100000}
VELT=${2:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT=${3:-$TARGET/bench/compile/scale-$LINES}
mkdir -p "$OUT"
if [ -z "$VELT" ]; then
  echo "building velt and the runtimes (release)..." >&2
  # velt_rt_shared too: debug builds link it (as an installed toolchain does) instead of the
  # static runtime, and timing them against the static one would measure another link.
  cargo build --release -q -p veltc -p velt_rt -p velt_rt_shared --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi
export VELT_STD=${VELT_STD:-$ROOT/std} VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off

python3 "$HERE/gen.py" "$LINES" "$OUT/src"

# run <label> <dir> <cmd...>: the command's rusage, wall time and stage lines (`velt: ...`).
run() {
  local label=$1 dir=$2; shift 2
  python3 - "$label" "$dir" "$OUT/$label.err" "$@" <<'PY'
import resource, subprocess, sys, time, re
label, cwd, err = sys.argv[1], sys.argv[2], sys.argv[3]
t0 = time.perf_counter()
with open(err, "w") as e:
    rc = subprocess.call(sys.argv[4:], cwd=cwd, stdout=subprocess.DEVNULL, stderr=e)
wall = time.perf_counter() - t0
ru = resource.getrusage(resource.RUSAGE_CHILDREN)
cpu = ru.ru_utime + ru.ru_stime
stages = [l.split()[1:3] for l in open(err) if re.match(r"velt: \S+\s+[\d.]+ ms", l)]
st = " ".join(f"{n}={float(ms)/1000:.2f}" for n, ms in stages if n != "total")
# ru_maxrss is in KB on Linux and in bytes on macOS.
rss_mb = ru.ru_maxrss / (1024 * 1024 if sys.platform == "darwin" else 1024)
print(f"{label:<14} rc={rc} cpu_s={cpu:.2f} wall_s={wall:.2f} maxrss_mb={rss_mb:.0f} {st}")
PY
}

fresh_home() { local h; h=$(mktemp -d "$OUT/home.XXXXXX"); echo "$h"; }
SRC="$OUT/src/velt"
VELT_HOME=$(fresh_home) run velt-check "$SRC" "$VELT" check -v main.vlt
VELT_HOME=$(fresh_home) run velt-debug "$SRC" "$VELT" build -v main.vlt -o "$OUT/velt-debug.bin"
VELT_HOME=$(fresh_home) run velt-release "$SRC" "$VELT" build --release -v main.vlt -o "$OUT/velt-release.bin"

GO=${GO:-$(command -v go || true)}
if [ -n "$GO" ]; then
  export GOCACHE=$(mktemp -d "$OUT/gocache.XXXXXX") GOFLAGS=-mod=mod GOTOOLCHAIN=local
  (cd "$OUT/src/go" && "$GO" build -o /dev/null std)
  run go-build "$OUT/src/go" "$GO" build -o "$OUT/go.bin" .
else
  echo "go: not found (set GO to compare with Go)"
fi
