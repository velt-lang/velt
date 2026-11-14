#!/usr/bin/env bash
# The edit loop's cost per stage (#2943 phase 0): `velt check`, a debug and a release build of
# each program, cold (empty build cache), warm (the same build again) and after a body edit (a
# `console.log` added before the first one in the program's file, or at the start of its first
# function when it has none), with the build stamp off so every build compiles.
#
#   bench/compile/edit_loop.sh VELT WORK [PROGRAM...]
#
# Prints one line per program, command and state: wall-clock per stage from `--timings` / `-v`
# (ms), and the instructions of `velt` and every process it starts:
# - macOS: `/usr/bin/time -l`;
# - Linux: `perf stat` when it is installed and allowed to count, else cachegrind with
#   `--trace-children=yes` (bench/compile/phases_instr.sh's method). Under cachegrind the
#   stage times are valgrind's, many times the real ones: compare the instructions only, and the
#   line says `timed=valgrind`.
# Wall-clock times are only comparable on an idle machine (CONTRIBUTING.md on measuring); the
# instructions are comparable under load.
#
# Programs default to the set on #2943: hello, sync and async fs, notes-cli, http_hello and
# todo-api (examples/apps/sigx-ssr/app/src/server.velt too, after `pnpm install` in its app
# directory). Each is built in place (path dependencies resolve) and its file is restored
# afterwards. A build that fails stops the script with its output and exit status 1.
set -euo pipefail
VELT=$(realpath "$1"); WORK=$2; shift 2
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$WORK"; WORK=$(realpath "$WORK")
export VELT_BUILD_STAMP=off
if [ $# -eq 0 ]; then
  set -- tests/golden/m1/hello.velt bench/compile/edit_loop/sync_fs.velt \
    bench/compile/edit_loop/async_fs.velt examples/apps/notes-cli/demo.velt \
    examples/http_hello.velt examples/apps/todo-api/demo.velt
fi

# How instructions are counted here: `time`, `perf`, `cachegrind` or `none`.
counter=none
case "$(uname)" in
  Darwin) counter=time;;
  Linux)
    if command -v perf >/dev/null && perf stat -x, -e instructions:u true 2>&1 >/dev/null \
      | grep -q '^[0-9]'; then
      counter=perf
    elif command -v valgrind >/dev/null; then
      counter=cachegrind
    fi;;
esac
if [ $counter = none ]; then
  echo "edit_loop.sh: no instruction counter here (no perf allowed to count, no valgrind):" \
    "the instructions column is '-'" >&2
fi

# Runs `velt "$@"` and prints its stages and instructions; fails with the build's output when the
# build fails.
measure() {
  local log=$WORK/log ins=- timed=wall
  rm -rf "$WORK/cg"; mkdir -p "$WORK/cg"
  case $counter in
    time)
      /usr/bin/time -l "$VELT" "$@" >/dev/null 2>"$log" || { cat "$log" >&2; return 1; }
      ins=$(awk '/instructions retired/ { print $1 }' "$log");;
    perf)
      perf stat -x, -e instructions:u -o "$log.perf" "$VELT" "$@" >/dev/null 2>"$log" \
        || { cat "$log" >&2; return 1; }
      ins=$(awk -F, '/instructions/ { print $1 }' "$log.perf");;
    cachegrind)
      valgrind --tool=cachegrind --cache-sim=no --trace-children=yes \
        --cachegrind-out-file=/dev/null --log-file="$WORK/cg/%p.log" "$VELT" "$@" \
        >/dev/null 2>"$log" || { cat "$log" >&2; return 1; }
      ins=$(cat "$WORK"/cg/*.log | sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' \
        | tr -d , | awk '{ s += $1 } END { print s }')
      timed=valgrind;;
    none) "$VELT" "$@" >/dev/null 2>"$log" || { cat "$log" >&2; return 1; };;
  esac
  awk -v ins="$ins" -v timed="$timed" '
    /^velt: [a-z]+ +[0-9.]+ ms/ && $2 != "verify" && $2 != "write" && $2 != "stamp" {
      printf "%s=%.0f ", $2, $3 }
    /^velt: std cache +[0-9.]+ ms/ { printf "std_cache=%.0f ", $4 }
    /std lines loaded/ { lines = $2 }
    /other lines loaded/ { other = $2 }
    END {
      printf "std_lines=%s other_lines=%s instructions=%s timed=%s\n", lines, other, ins, timed
    }' "$log"
}

# A body edit: a `console.log` before the program's first one, else at the start of its first
# function.
edit() {
  perl -0pi -e 'unless (s/console\.log\(/console.log("edit $$"); console.log(/) {
    s/(\bfunction\s+\w+\s*\([^)]*\)[^{\n]*\{)/$1 console.log("edit $$");/ }' "$1"
}

for program in "$@"; do
  file=$ROOT/$program
  dir=$(dirname "$file"); name=$(basename "$file" .velt)
  cp "$file" "$WORK/$name.orig"
  trap 'cp "$WORK/$name.orig" "$file"' EXIT
  for kind in check debug release; do
    export VELT_BUILD_CACHE=$WORK/cache-$kind
    rm -rf "$VELT_BUILD_CACHE"
    case $kind in
      check) args=(check -v "$file");;
      debug) args=(build "$file" -o "$WORK/$name.debug" --timings);;
      release) args=(build --release "$file" -o "$WORK/$name.release" --timings);;
    esac
    cd "$dir"
    for state in cold warm edit; do
      [ $state = edit ] && edit "$file"
      # A failed build stops here: `measure`'s status is lost inside `echo "$(…)"`.
      row=$(measure "${args[@]}") || { echo "edit_loop.sh: $program $kind $state failed" >&2; exit 1; }
      echo "$program $kind $state $row"
    done
    cp "$WORK/$name.orig" "$file"
  done
  trap - EXIT
done
