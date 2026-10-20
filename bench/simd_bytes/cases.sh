#!/usr/bin/env bash
# `u8[]` `indexOf` and `TextDecoder.decode` (search.vlt, decode.vlt) at the array lengths and
# kinds of text where SIMD code and the runtime's memchr trade places, and the bench/stdport
# rows on them, for two git refs:
#
#   bench/simd_bytes/cases.sh BASE_REF HEAD_REF WORK_DIR [RUNS]
#
# Exports each ref into WORK_DIR/{base,head}, builds velt (release) in WORK_DIR/target-<side>,
# builds the programs with the LLVM backend, and prints a Markdown table per case: the
# instructions executed (cachegrind, `VELT_THREADS=1`), which repeat exactly, and the best
# wall time (ms) of RUNS interleaved runs (default 3). Run it from inside the repository, or
# with GIT_DIR set. Needs: cargo, clang, valgrind, python3 (Linux).
set -euo pipefail
if [ $# -lt 3 ]; then
  echo "usage: bench/simd_bytes/cases.sh BASE_REF HEAD_REF WORK_DIR [RUNS]" >&2
  exit 2
fi
BASE_REF=$1
HEAD_REF=$2
WORK=$3
RUNS=${4:-3}
mkdir -p "$WORK"
WORK=$(cd "$WORK" && pwd)
for side in base head; do
  ref=$BASE_REF
  [ "$side" = head ] && ref=$HEAD_REF
  rm -rf "${WORK:?}/$side"
  mkdir -p "$WORK/$side"
  git archive "$ref" | tar -x -C "$WORK/$side"
  echo "$side: $(git rev-parse "$ref")" >&2
done
for side in base head; do
  CARGO_TARGET_DIR="$WORK/target-$side" cargo build --release -q -p veltc -p velt_rt \
    --manifest-path "$WORK/$side/Cargo.toml"
  # The programs of HEAD_REF, so both sides run the same source.
  for prog in simd_bytes/search simd_bytes/decode stdport/bytes_search stdport/utf8_decode \
    stdport/utf8_valid; do
    "$WORK/target-$side/release/velt" build --release --backend llvm \
      "$WORK/head/bench/$prog.vlt" -o "$WORK/$(basename "$prog")-$side"
  done
done

python3 - "$WORK" "$RUNS" <<'PY'
import os, re, subprocess, sys, time
work, runs = sys.argv[1], int(sys.argv[2])
env = dict(os.environ, VELT_THREADS="1")
cases = [
    ("search", ["16", "5000000", "-1"]),
    ("search", ["64", "2000000", "-1"]),
    ("search", ["128", "1000000", "-1"]),
    ("search", ["208", "1000000", "-1"]),
    ("search", ["256", "1000000", "-1"]),
    ("search", ["512", "500000", "-1"]),
    ("search", ["4096", "200000", "-1"]),
    ("search", ["4096", "2000000", "20"]),
    ("search", ["1048576", "500", "-1"]),
    ("decode", ["ascii", "40", "500000"]),
    ("decode", ["ascii", "1048576", "20"]),
    ("decode", ["cyrillic", "40", "500000"]),
    ("decode", ["cyrillic", "1048576", "20"]),
    ("decode", ["comma", "1048576", "20"]),
    ("decode", ["latin", "1048576", "20"]),
    ("bytes_search", []),
    ("utf8_decode", []),
    ("utf8_valid", []),
]
def run(side, prog, argv):
    out = subprocess.run([f"{work}/{prog}-{side}"] + argv, env=env, check=True,
                         capture_output=True, text=True).stdout
    return out
def instructions(side, prog, argv):
    log = f"{work}/cg.log"
    subprocess.run(["valgrind", "--tool=cachegrind", "--cache-sim=no",
                    f"--cachegrind-out-file=/dev/null", f"--log-file={log}",
                    f"{work}/{prog}-{side}"] + argv, env=env, check=True, capture_output=True)
    m = re.search(r"I\s+refs:\s+([\d,]+)", open(log).read())
    return int(m.group(1).replace(",", ""))
print("| case | base instr | head instr | head/base | base ms | head ms |")
print("|---|---:|---:|---:|---:|---:|")
for prog, argv in cases:
    if run("base", prog, argv) != run("head", prog, argv):
        sys.exit(f"{prog} {argv}: the two sides print different output")
    ins = {s: instructions(s, prog, argv) for s in ("base", "head")}
    best = {"base": 1e18, "head": 1e18}
    for _ in range(runs):
        for s in ("base", "head"):
            t = time.perf_counter()
            run(s, prog, argv)
            best[s] = min(best[s], (time.perf_counter() - t) * 1000)
    print(f"| {prog} {' '.join(argv)} | {ins['base']/1e6:,.1f} M | {ins['head']/1e6:,.1f} M | "
          f"{ins['head']/ins['base']:.2f}x | {best['base']:.0f} | {best['head']:.0f} |", flush=True)
PY
