#!/usr/bin/env bash
# Directory listing: a walk of 18,000 files that stats every entry (list_names) against one that
# reads the types from the listing (list_typed, `readDirEntriesSync`).
#
#   bench/fs/run.sh [--velt PATH] [--dir TREE] [--runs N]
#
# Builds both walkers (LLVM, --release), creates the tree under --dir (default: a directory in
# the build directory; kept for later runs), then prints, per walker, the best wall time of N runs
# (default 5, after one warm-up run, so the tree is in the OS cache) and, where valgrind is
# installed, the instructions executed in user space (cachegrind: a stat's cost is mostly in the
# kernel, which it does not count, so the wall time shows more of the difference).
# Needs: a built velt (or cargo), clang, python3; valgrind for the counts.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
PYTHON=$(command -v python3 || command -v python)
TARGET=${CARGO_TARGET_DIR:-$ROOT/target}
OUT="$TARGET/bench-fs"
VELT=
TREE=
RUNS=5
while [ $# -gt 0 ]; do
  case "$1" in
    --velt) VELT=$2; shift 2 ;;
    --dir) TREE=$2; shift 2 ;;
    --runs) RUNS=$2; shift 2 ;;
    *) echo "usage: bench/fs/run.sh [--velt PATH] [--dir TREE] [--runs N]" >&2; exit 2 ;;
  esac
done
mkdir -p "$OUT"
TREE=${TREE:-$OUT/tree}
if [ -z "$VELT" ]; then
  echo "building velt (release) and the runtime..." >&2
  cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
  VELT="$TARGET/release/velt"
fi
EXE=
case "$(uname -s)" in MINGW* | MSYS* | CYGWIN*) EXE=.exe ;; esac

for name in make_tree list_names list_typed; do
  "$VELT" build --release --backend llvm "$HERE/$name.vlt" -o "$OUT/$name$EXE" >&2
done
mkdir -p "$TREE"
"$OUT/make_tree$EXE" "$TREE"

"$PYTHON" - "$RUNS" "$TREE" "$OUT/list_names$EXE" "$OUT/list_typed$EXE" <<'PY'
import re, shutil, subprocess, sys, time
runs, tree, programs = int(sys.argv[1]), sys.argv[2], sys.argv[3:]
grind = shutil.which("valgrind")
print("| walker | files listed | best of %d (ms) | median (ms) | instructions (M) |" % runs)
print("|---|---:|---:|---:|---:|")
for exe in programs:
    name = re.sub(r"(\.exe)?$", "", exe.replace("\\", "/").rsplit("/", 1)[-1])
    out = subprocess.run([exe, tree], check=True, capture_output=True, text=True).stdout.strip()
    times = []
    for _ in range(runs):
        t = time.perf_counter()
        subprocess.run([exe, tree], check=True, stdout=subprocess.DEVNULL)
        times.append((time.perf_counter() - t) * 1000)
    times.sort()
    instr = "-"
    if grind:
        r = subprocess.run([grind, "--tool=cachegrind", "--cache-sim=no",
                            "--cachegrind-out-file=/dev/null", exe, tree],
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        m = re.search(r"I\s+refs:\s+([\d,]+)", r.stderr)
        if r.returncode != 0 or not m:
            sys.exit(f"{name}: cachegrind failed (exit {r.returncode}):\n{r.stderr[-2000:]}")
        instr = "%.1f" % (int(m.group(1).replace(",", "")) / 1e6)
    print(f"| {name} | {out} | {times[0]:.1f} | {times[len(times) // 2]:.1f} | {instr} |")
PY
