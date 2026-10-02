#!/usr/bin/env bash
# Benchmarks Game runner (macOS / Linux). Builds every implementation of every program in
# bench/benchmarks-game/<program>/ (layout: README.md), checks that each one's output matches
# byte-for-byte, and prints a Markdown table of best-of-RUNS wall time, CPU time and peak RSS.
#
#   bench/benchmarks-game/run.sh [--quick] [--runs N] [program...]
#
#   --quick   run QUICK_N and compare with expected-quick.txt (the official outputs)
#   --runs N  timed runs per implementation (default 3; repetition stops once a run exceeds 10 s)
#
# Velt is built with `--release --backend llvm` and `--release --backend cranelift`; Rust from the
# Cargo project in rust/ (release, LTO); Go with `go build`; Node and Bun run the same main*.js.
# Full runs compare every implementation with the Rust output. Needs cargo, clang, go, node, bun,
# python3. Missing tools or failed builds show as `n/a` with the reason on stderr.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT="$ROOT/target/bench-game"
QUICK=0
RUNS=3
PROGRAMS=()
while [[ $# -gt 0 ]]; do
  case $1 in
    --quick) QUICK=1 ;;
    --runs) RUNS=$2; shift ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) PROGRAMS+=("$1") ;;
  esac
  shift
done
if [[ ${#PROGRAMS[@]} -eq 0 ]]; then
  for d in "$HERE"/*/bench.conf; do PROGRAMS+=("$(basename "$(dirname "$d")")"); done
fi
mkdir -p "$OUT/bin"

echo "building velt (release), the runtime and the Rust programs..." >&2
cargo build --release -q -p veltc -p velt_rt --manifest-path "$ROOT/Cargo.toml"
PYTHON=$(command -v python3)
# velt is where cargo put it: CARGO_TARGET_DIR, a cargo config, or <repo>/target.
VELT=$(cargo metadata --format-version 1 --no-deps --manifest-path "$ROOT/Cargo.toml" |
  "$PYTHON" -c 'import json, sys; sys.stdout.write(json.load(sys.stdin)["target_directory"])')/release/velt
RUST_TARGET="$OUT/rust-target"
RUSTFLAGS="-C target-cpu=native" cargo build --release -q \
  --manifest-path "$HERE/rust/Cargo.toml" --target-dir "$RUST_TARGET"
RUST_BIN="$RUST_TARGET/release"

# fasta_input <n>: path of the fasta output for n (the stdin of k-nucleotide, reverse-complement, …).
fasta_input() {
  local file="$OUT/fasta-$1.txt"
  [[ -s "$file" ]] || "$RUST_BIN/fasta" "$1" > "$file"
  echo "$file"
}

# measure <expected> <stdin> <command...>: prints "wall_s cpu_s rss_mb" (best of RUNS each, after an
# untimed checking run) or fails when the output differs from <expected>. rusage comes from wait4, so it is per child process.
measure() {
  "$PYTHON" - "$RUNS" "$@" <<'EOF'
import os, subprocess, sys, tempfile, time
runs, expected, stdin, cmd = int(sys.argv[1]), sys.argv[2], sys.argv[3], sys.argv[4:]

def run():
    with open(stdin, "rb") as inp, tempfile.TemporaryFile() as out:
        t = time.perf_counter()
        p = subprocess.Popen(cmd, stdin=inp, stdout=out, stderr=subprocess.DEVNULL)
        _, status, ru = os.wait4(p.pid, 0)
        wall = time.perf_counter() - t
        if status != 0:
            sys.exit(f"{' '.join(cmd)}: exit status {status}")
        out.seek(0)
        return wall, ru, out.read()

# The untimed first run checks the output and absorbs macOS's first-launch scan of new binaries.
if run()[2] != open(expected, "rb").read():
    sys.exit(f"{' '.join(cmd)}: output differs from {expected}")
best = [float("inf")] * 3
for _ in range(runs):
    wall, ru, _ = run()
    rss = ru.ru_maxrss / (1 << 20) if sys.platform == "darwin" else ru.ru_maxrss / 1024
    best = [min(best[0], wall), min(best[1], ru.ru_utime + ru.ru_stime), min(best[2], rss)]
    if wall > 10:
        break
print(f"{best[0]:.3f} {best[1]:.3f} {best[2]:.1f}")
EOF
}

# impls <program>: lines "label|build-kind|source" for every implementation present.
impls() {
  local dir="$HERE/$1" f stem
  for f in "$RUST_BIN/$1" "$RUST_BIN/$1"_*; do
    [[ -x "$f" && ! -d "$f" && $f != *.d ]] && echo "Rust ${f##*/}|rust|$f"
  done
  for f in "$dir"/main*.vlt; do
    [[ -e "$f" ]] || continue
    stem=$(basename "$f" .vlt)
    echo "Velt LLVM $stem|velt-llvm|$f"
    echo "Velt Cranelift $stem|velt-cranelift|$f"
  done
  for f in "$dir"/main*.go; do [[ -e "$f" ]] && echo "Go $(basename "$f" .go)|go|$f"; done
  for f in "$dir"/main*.js; do
    [[ -e "$f" ]] || continue
    echo "Node $(basename "$f" .js)|node|$f"
    echo "Bun $(basename "$f" .js)|bun|$f"
  done
}

# command_for <program> <kind> <source> <n>: builds if needed, prints the command to run.
command_for() {
  local exe="$OUT/bin/$1-$2-$(basename "${3%.*}")"
  case $2 in
    rust) echo "$3 $4" ;;
    velt-llvm | velt-cranelift)
      "$VELT" build --release --backend "${2#velt-}" "$3" -o "$exe" >&2 && echo "$exe $4" ;;
    go) (cd "$(dirname "$3")" && go build -o "$exe" "$(basename "$3")") >&2 && echo "$exe $4" ;;
    node | bun) command -v "$2" > /dev/null && echo "$2 $3 $4" ;;
  esac
}

echo "| program | implementation | wall s | CPU s | peak RSS MB | wall × Rust | CPU × Rust 1-thread |"
echo "|---|---|---:|---:|---:|---:|---:|"
for program in "${PROGRAMS[@]}"; do
  N="" QUICK_N="" STDIN=none
  # shellcheck source=/dev/null
  source "$HERE/$program/bench.conf"
  n=$N
  [[ $QUICK -eq 1 ]] && n=$QUICK_N
  stdin=/dev/null
  [[ $STDIN == fasta ]] && { stdin=$(fasta_input "$n"); n=""; }
  if [[ $QUICK -eq 1 ]]; then
    expected="$HERE/$program/expected-quick.txt"
  else
    expected="$OUT/$program-$N.expected"
    # shellcheck disable=SC2086 # n is empty for stdin programs
    "$RUST_BIN/$program" $n < "$stdin" > "$expected"
  fi
  rust_wall="" rust_st_cpu=""
  while IFS='|' read -r label kind src; do
    if ! cmd=$(command_for "$program" "$kind" "$src" "$n"); then
      echo "| $program | $label | n/a | | | | |"
      continue
    fi
    # shellcheck disable=SC2086 # the command is meant to split
    if ! result=$(measure "$expected" "$stdin" $cmd); then
      echo "| $program | $label | n/a | | | | |"
      continue
    fi
    read -r wall cpu rss <<< "$result"
    # The single-threaded reference is <program>_st when the fastest Rust program is parallel.
    [[ $label == "Rust $program" ]] && rust_wall=$wall rust_st_cpu=$cpu
    [[ $label == "Rust ${program}_st" ]] && rust_st_cpu=$cpu
    ratio="" cpu_ratio=""
    [[ -n $rust_wall ]] && ratio=$("$PYTHON" -c "print(f'{$wall / $rust_wall:.2f}')")
    [[ -n $rust_st_cpu && $kind != rust ]] && cpu_ratio=$("$PYTHON" -c "print(f'{$cpu / $rust_st_cpu:.2f}')")
    echo "| $program | $label | $wall | $cpu | $rss | $ratio | $cpu_ratio |"
  done < <(impls "$program")
done
echo
echo "Best of $RUNS runs (wall and CPU seconds, peak RSS), $([[ $QUICK -eq 1 ]] && echo quick || echo official) sizes."
