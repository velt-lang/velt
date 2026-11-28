#!/usr/bin/env bash
# The stdout writer in std (#2196), base against head (Linux): instructions by cachegrind.
#
#   bench/stdout_writer_ab.sh BASE_VELT BASE_STD HEAD_VELT HEAD_STD [WORK]
#
# Each side is a release `velt` with its own std (and the runtime next to it, as a cargo target
# directory holds it, or an installed toolchain's layout: bench/stdout_writer_prefix.sh makes one
# from a checkout, with the writer's shipped object). Rows, each `name base head change`:
# - run: a release build of hello, and of 100,000 `console.log` lines (bench/stdout_lines.velt),
#   its stdout to a file and to a pipe; VELT_THREADS=1.
# - compile: `velt build` of hello and of a generated 10,000-line program (compile/scale/gen.py),
#   release and debug, with the build cache cold (empty, every time) and warm (filled by the
#   same build first), the velt process alone (`velt-only`) and with the processes it starts,
#   clang and the linker (`all`).
# - prebuilt: a release and a debug build of bench/stdport/to_fixed.velt (its number printing is
#   a prebuilt hook module), the cache warm with the prebuilt objects in it.
# Needs valgrind and python3.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
BV=${1:?base velt}
BS=${2:?base std}
HV=${3:?head velt}
HS=${4:?head std}
W=${5:-$PWD/stdout-writer-ab}
mkdir -p "$W/src" "$W/cg"
export VELT_THREADS=1

printf 'function main() {\n  console.log("hi");\n}\n' > "$W/src/hello.velt"
cp "$HERE/stdout_lines.velt" "$W/src/lines.velt"
cp "$HERE/stdport/to_fixed.velt" "$W/src/to_fixed.velt"
[[ -d $W/src/app10k ]] || python3 -I "$HERE/compile/scale/gen.py" 10000 "$W/src/app10k" > /dev/null

# instr <file>: the instructions cachegrind counted (all processes it followed).
instr() {
  python3 -I - "$@" <<'PY'
import glob, sys
total = 0
for path in glob.glob(sys.argv[1] + "*"):
    for line in open(path):
        if line.startswith("summary:"):
            total += int(line.split()[1])
print(total)
PY
}

# cg <out prefix> <children yes|no> [VAR=value...] <cmd...>: run under cachegrind, with the
# environment variables given.
cg() {
  local out=$1 children=$2
  shift 2
  local vars=()
  while [[ $1 == *=* ]]; do
    vars+=("$1")
    shift
  done
  rm -f "$out".*
  env "${vars[@]}" valgrind --tool=cachegrind --cache-sim=no --trace-children="$children" \
    --cachegrind-out-file="$out.%p" "$@" > /dev/null 2>&1
}

row() {
  local name=$1 b h
  b=$(instr "$W/cg/$name-base.")
  h=$(instr "$W/cg/$name-head.")
  python3 -I -c 'import sys; n,b,h=sys.argv[1],int(sys.argv[2]),int(sys.argv[3]); print(f"{n} {b} {h} {100*(h-b)/b:+.2f}%")' "$name" "$b" "$h"
}

side() { [[ $1 == base ]] && echo "$BV $BS" || echo "$HV $HS"; }

# Run rows.
if [[ -z ${SKIP_RUN:-} ]]; then
for s in base head; do
  read -r velt std <<< "$(side $s)"
  for p in hello lines; do
    VELT_STD=$std VELT_BUILD_STAMP=off "$velt" build --release "$W/src/$p.velt" -o "$W/$p-$s" 2> /dev/null
  done
  cg "$W/cg/run-hello-$s" no "$W/hello-$s"
  rm -f "$W/cg/run-lines-file-$s".*
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$W/cg/run-lines-file-$s.%p" \
    "$W/lines-$s" > "$W/lines-$s.out" 2> /dev/null
  rm -f "$W/cg/run-lines-pipe-$s".*
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file="$W/cg/run-lines-pipe-$s.%p" \
    "$W/lines-$s" 2> /dev/null | cat > /dev/null
done
cmp -s "$W/lines-base.out" "$W/lines-head.out" || echo "lines: the outputs differ"
for r in run-hello run-lines-file run-lines-pipe; do row "$r"; done
fi

# Compile rows.
for prog in hello app10k; do
  input=$W/src/$prog.velt
  [[ $prog == app10k ]] && input=$W/src/app10k/velt/main.velt
  for mode in release debug; do
    flags=(); [[ $mode == release ]] && flags=(--release)
    for cache in cold warm; do
      for children in no yes; do
        name=compile-$prog-$mode-$cache-$([[ $children == no ]] && echo velt-only || echo all)
        for s in base head; do
          read -r velt std <<< "$(side $s)"
          c=$W/cache-$s-$prog-$mode-$cache-$children
          rm -rf "$c"
          env=(VELT_STD="$std" VELT_BUILD_STAMP=off VELT_BUILD_CACHE="$c")
          if [[ $cache == warm ]]; then
            env "${env[@]}" "$velt" build "${flags[@]}" "$input" -o "$W/out-$s" 2> /dev/null
          fi
          cg "$W/cg/$name-$s" "$children" "${env[@]}" "$velt" build "${flags[@]}" "$input" -o "$W/out-$s"
        done
        row "$name"
      done
    done
  done
done

# Prebuilt hook rows: the cache holds the objects (filled by a first build).
for mode in release debug; do
  flags=(); [[ $mode == release ]] && flags=(--release)
  name=prebuilt-to_fixed-$mode
  for s in base head; do
    read -r velt std <<< "$(side $s)"
    c=$W/cache-$s-prebuilt-$mode
    rm -rf "$c"
    env=(VELT_STD="$std" VELT_BUILD_STAMP=off VELT_BUILD_CACHE="$c")
    env "${env[@]}" VELT_PREBUILT_HOOKS=fill "$velt" build "${flags[@]}" "$W/src/to_fixed.velt" -o "$W/tf-$s" 2> /dev/null
    cg "$W/cg/$name-$s" no "${env[@]}" "$velt" build "${flags[@]}" "$W/src/to_fixed.velt" -o "$W/tf-$s"
  done
  row "$name"
done
