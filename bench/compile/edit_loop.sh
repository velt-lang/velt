#!/usr/bin/env bash
# The edit loop's cost per stage (#2943 phase 0): `velt check`, a debug and a release build of
# each program, cold (empty build cache), warm (the same build again) and after a body edit (a
# `console.log` added before the first one in the program's file), with the build stamp off so
# every build compiles.
#
#   bench/compile/edit_loop.sh VELT WORK [PROGRAM...]
#
# Prints one line per program, command and state: wall-clock per stage from `--timings` / `-v`
# (ms), and instructions of the whole process tree where the OS counts them (`/usr/bin/time -l`
# on macOS, `perf stat` on Linux; `-` otherwise). Compare runs on an idle machine, or by
# instructions (CLAUDE.md "Measuring on a busy machine").
#
# Programs default to the set on #2943: hello, sync and async fs, notes-cli, http_hello and
# todo-api. Each is built in place (path dependencies resolve) and its file restored afterwards.
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

# Runs `velt "$@"` and prints its stages and instructions.
measure() {
  local log=$WORK/log ins=-
  case "$(uname)" in
    Darwin)
      /usr/bin/time -l "$VELT" "$@" >/dev/null 2>"$log" || { cat "$log" >&2; return 1; }
      ins=$(awk '/instructions retired/ { print $1 }' "$log");;
    Linux)
      if command -v perf >/dev/null; then
        perf stat -x, -e instructions:u -o "$log.perf" "$VELT" "$@" >/dev/null 2>"$log" \
          || { cat "$log" >&2; return 1; }
        ins=$(awk -F, '/instructions/ { print $1 }' "$log.perf")
      else
        "$VELT" "$@" >/dev/null 2>"$log" || { cat "$log" >&2; return 1; }
      fi;;
    *) "$VELT" "$@" >/dev/null 2>"$log" || { cat "$log" >&2; return 1; };;
  esac
  awk -v ins="$ins" '
    /^velt: [a-z]+ +[0-9.]+ ms/ && $2 != "verify" && $2 != "write" && $2 != "stamp" {
      printf "%s=%.0f ", $2, $3 }
    /^velt: std cache +[0-9.]+ ms/ { printf "std_cache=%.0f ", $4 }
    /std lines loaded/ { lines = $2 }
    /other lines loaded/ { other = $2 }
    END { printf "std_lines=%s other_lines=%s instructions=%s\n", lines, other, ins }' "$log"
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
    (cd "$dir"
      echo "$program $kind cold $(measure "${args[@]}")"
      echo "$program $kind warm $(measure "${args[@]}")"
      perl -pi -e 'if (!$d && s/console\.log\(/console.log("edit $$"); console.log(/) { $d = 1 }' "$file"
      echo "$program $kind edit $(measure "${args[@]}")")
    cp "$WORK/$name.orig" "$file"
  done
  trap - EXIT
done
