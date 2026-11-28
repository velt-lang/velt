#!/usr/bin/env bash
# Instruction counts (cachegrind, one thread) of two trees' compilers and programs (Linux):
#
#   bench/strings_utf16/instr_rows.sh BASE_TREE HEAD_TREE WORK [PART...]
#
# Each tree is a checkout with a release build (`target/release/velt` and `libvelt_rt.a`:
# `cargo build --release -p veltc --bin velt`, then `-p velt_rt -p velt_rt_shared`), each with
# its own std. Prints, for each side and the change:
# - `check` and `build` of hello (tests/golden/m1/hello.velt) and of a generated 10k-line app
#   (bench/compile/scale/app.py), with a build cache of each tree's own that one build of the same
#   program filled first ("cache": std's prebuilt objects present), and with the cache off
#   ("off": every std module the program uses is checked and compiled). Builds are velt's own
#   instructions: cachegrind doesn't follow clang and the linker.
# - the run of each bench/strings_utf16 part named (default: slice char_code_at for_of csv),
#   built `--release` from HEAD_TREE's source by each side's compiler with its cache (objects).
set -euo pipefail
if [ $# -lt 3 ]; then
  echo "usage: bench/strings_utf16/instr_rows.sh BASE_TREE HEAD_TREE WORK [PART...]" >&2
  exit 2
fi
BASE=$(cd "$1" && pwd)
HEAD=$(cd "$2" && pwd)
mkdir -p "$3"
WORK=$(cd "$3" && pwd)
shift 3
PARTS=("$@")
[ ${#PARTS[@]} -eq 0 ] && PARTS=(slice char_code_at for_of csv)

[ -f "$WORK/app10k/main.velt" ] || python3 "$HEAD/bench/compile/scale/app.py" 10000 "$WORK/app10k" >/dev/null
cp "$HEAD/tests/golden/m1/hello.velt" "$WORK/hello.velt"

ir() { # <tree> <cache> <args...>: velt's instructions
  local tree=$1 cache=$2
  shift 2
  VELT_THREADS=1 VELT_STD="$tree/std" VELT_RT_LIB="$tree/target/release/libvelt_rt.a" \
    VELT_BUILD_CACHE="$cache" valgrind --tool=cachegrind --cache-sim=no \
    --cachegrind-out-file=/dev/null "$tree/target/release/velt" "$@" 2>&1 >/dev/null |
    sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,
}

row() { # <name> <base count> <head count>
  awk -v n="$1" -v b="$2" -v h="$3" 'BEGIN { printf "%-28s %14d %14d %+7.2f%%\n", n, b, h, (h - b) * 100 / b }'
}

printf "%-28s %14s %14s %8s\n" row base head change
for prog in hello app10k; do
  src=$WORK/hello.velt
  [ $prog = app10k ] && src=$WORK/app10k/main.velt
  for mode in cache off; do
    counts=()
    for side in base head; do
      tree=$BASE
      [ $side = head ] && tree=$HEAD
      cache=off
      if [ $mode = cache ]; then
        cache=$WORK/cache-$side
        VELT_PREBUILT_HOOKS=fill VELT_STD="$tree/std" VELT_RT_LIB="$tree/target/release/libvelt_rt.a" VELT_BUILD_CACHE="$cache" \
          "$tree/target/release/velt" build "$src" -o "$WORK/$prog-$side-warm" >/dev/null
      fi
      c=$(ir "$tree" "$cache" check "$src")
      # An output of its own: a build finding its output up to date (the build stamp) does nothing.
      rm -f "$WORK/$prog-$side-$mode"
      b=$(ir "$tree" "$cache" build "$src" -o "$WORK/$prog-$side-$mode")
      counts+=("$c" "$b")
    done
    row "$prog check ($mode)" "${counts[0]}" "${counts[2]}"
    row "$prog build ($mode)" "${counts[1]}" "${counts[3]}"
  done
done

for part in "${PARTS[@]}"; do
  src=$HEAD/bench/strings_utf16/$part.velt
  counts=()
  for side in base head; do
    tree=$BASE
    [ $side = head ] && tree=$HEAD
    exe=$WORK/$part-$side
    # Twice: the second build finds std's objects the first one left in the cache.
    for _ in 1 2; do
      VELT_PREBUILT_HOOKS=fill VELT_STD="$tree/std" VELT_RT_LIB="$tree/target/release/libvelt_rt.a" \
        VELT_BUILD_CACHE="$WORK/cache-$side" \
        "$tree/target/release/velt" build --release "$src" -o "$exe" >/dev/null
    done
    out=$(VELT_THREADS=1 "$exe")
    [ "$out" = "$(node "$HEAD/bench/strings_utf16/$part.js")" ] || echo "$part ($side): output differs from Node" >&2
    counts+=("$(VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$exe" 2>&1 >/dev/null |
      sed -n 's/.*I[[:space:]]*refs:[[:space:]]*\([0-9,]*\).*/\1/p' | tr -d ,)")
  done
  row "run $part" "${counts[0]}" "${counts[1]}"
done
