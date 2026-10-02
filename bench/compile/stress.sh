#!/usr/bin/env bash
# Compile-time memory stress run (Linux / macOS; stress.ps1 is the Windows counterpart): a debug
# build (Cranelift) of long_main_<CLASSES> (CLASSES classes with an override and a generic instance
# each, all used from one `main` of 6 × CLASSES blocks; the same program as run.sh's long_main)
# must stay under LIMIT_MB of peak memory (resident set of the whole `velt build`). Prints the
# stage times, the peak and the verdict; exits 1 over the limit.
#
#   bench/compile/stress.sh [classes = 16000] [limit in MB = 2048] [path to velt]
#
# Without a velt path it builds velt (release) first. Needs python3 (or python) on PATH.
set -euo pipefail
CLASSES=${1:-16000}
LIMIT_MB=${2:-2048}
VELT=${3:-}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT="$TARGET/bench/compile"
mkdir -p "$OUT"
PYTHON=$(command -v python3 || command -v python)

if [ -z "$VELT" ]; then
  echo "building velt and the runtimes (release)..." >&2
  cargo build --release -q -p veltc -p velt_rt -p velt_rt_shared --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi

"$PYTHON" - "$CLASSES" "$LIMIT_MB" "$VELT" "$OUT" <<'PY'
import os, resource, subprocess, sys
n, limit, velt, out = int(sys.argv[1]), int(sys.argv[2]), sys.argv[3], sys.argv[4]
parts = ["function count<T>(xs: T[]): i64 {\n  return xs.length as i64;\n}\n\n"]
for i in range(n):
    parts.append(f"class C{i} {{\n  id: i64;\n  constructor(id: i64) {{\n    this.id = id;\n  }}\n"
                 f"  area(): i64 {{\n    return this.id;\n  }}\n}}\n\n")
    parts.append(f"class S{i} extends C{i} {{\n  constructor(id: i64) {{\n    super(id);\n  }}\n"
                 f"  override area(): i64 {{\n    return this.id + 1;\n  }}\n}}\n\n")
parts.append("function main() {\n  let t = 0;\n")
parts += [f"  const b{i}: C{i} = new S{i}({i});\n  t += b{i}.area() + count([b{i}]);\n" for i in range(n)]
path = f"{out}/long_main_{n}.vlt"
open(path, "w").write("".join(parts) + "  console.log(t);\n}\n")
exe = f"{out}/long_main_{n}_stress"
for stale in (exe, exe + ".link-stamp"):
    if os.path.exists(stale):
        os.remove(stale)
r = subprocess.run([velt, "build", "-v", path, "-o", exe], stdout=subprocess.DEVNULL,
                   stderr=subprocess.PIPE, text=True)
sys.stderr.write(r.stderr)
if r.returncode != 0:
    sys.exit(f"velt build {path} failed")
# ru_maxrss: KiB on Linux, bytes on macOS.
peak = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
peak_mb = peak / (1 << 20) if sys.platform == "darwin" else peak / 1024
expected = f"{sum(i + 2 for i in range(n))}\n"
got = subprocess.run([exe], stdout=subprocess.PIPE, text=True).stdout
if got != expected:
    sys.exit(f"long_main_{n} printed {got!r}, expected {expected!r}")
verdict = "ok" if peak_mb <= limit else "OVER THE LIMIT"
print(f"long_main_{n}, debug build: peak memory {peak_mb:.0f} MB (limit {limit} MB): {verdict}")
sys.exit(0 if peak_mb <= limit else 1)
PY
