#!/usr/bin/env bash
# Generic instances shared by shape (docs/internals/design/shape-sharing.md): a program whose
# `any` arrays are converted to N element types (gen.py), built in debug with sharing on and
# off (`VELT_SHAPES=off`). Prints, for each, the VIR body lines of the whole program and of the
# array-view helpers, the shared instances (`VELT_LOWER_PROFILE`), build CPU (user + sys) and
# peak memory, with a fresh empty VELT_HOME and the build caches off.
#
#   bench/compile/shapes/run.sh [object types = 40] [path to velt] [out dir]
#
# Without a velt path it builds velt (release) first. Needs python3.
set -euo pipefail
N=${1:-40}
VELT=${2:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT=${3:-$TARGET/bench/compile/shapes-$N}
mkdir -p "$OUT"
if [ -z "$VELT" ]; then
  echo "building velt and the runtime (release)..." >&2
  cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi
export VELT_STD=${VELT_STD:-$ROOT/std} VELT_BUILD_CACHE=off VELT_BUILD_STAMP=off
python3 "$HERE/gen.py" "$N" "$OUT/shapes.vlt"

# body_lines <vir file> <pattern>: non-empty lines inside the bodies of functions whose symbol
# matches the pattern (all functions for "").
body_lines() {
  python3 - "$1" "$2" <<'PY'
import re, sys
path, pat = sys.argv[1], sys.argv[2]
hdr = re.compile(r'^fn#\d+ \w+ (\S+?)\(')
cur, n = None, 0
for line in open(path, errors='replace'):
    m = hdr.match(line)
    if m:
        cur = m.group(1)
        continue
    if line.startswith('}'):
        cur = None
    elif cur is not None and line.strip() and pat in cur:
        n += 1
print(n)
PY
}

# measure <label> <env...>: CPU seconds and peak RSS of a debug build.
measure() {
  local label=$1; shift
  env "$@" VELT_HOME="$(mktemp -d)" python3 - "$VELT" "$OUT" <<'PY'
import resource, subprocess, sys
velt, out = sys.argv[1], sys.argv[2]
rc = subprocess.call([velt, "build", "shapes.vlt", "-o", "shapes.bin"], cwd=out,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
r = resource.getrusage(resource.RUSAGE_CHILDREN)
print(f"cpu {r.ru_utime + r.ru_stime:.2f} s, peak {r.ru_maxrss // 1024} MB" + ("" if rc == 0 else f" (exit {rc})"))
PY
}

for mode in off on; do
  if [ "$mode" = off ]; then set -- VELT_SHAPES=off; else set -- VELT_SHAPES=; fi
  (cd "$OUT" && env "$@" VELT_LOWER_PROFILE=1 "$VELT" build shapes.vlt --emit vir \
    >"vir-$mode.txt" 2>"profile-$mode.txt")
  total=$(body_lines "$OUT/vir-$mode.txt" "")
  helpers=$(body_lines "$OUT/vir-$mode.txt" "array__viewN")
  shared=$(grep "shared instances" "$OUT/profile-$mode.txt" | tail -1 | sed 's/.*: //' || true)
  echo "sharing $mode: VIR $total lines, helpers $helpers; ${shared:-no shared instances}; $(measure "$mode" "$@")"
done
