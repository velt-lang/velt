#!/bin/bash
# pr-ab.sh <worktree> <base velt> <head velt> [units]: instructions of warm `velt check` and
# `velt build` of hello, examples/todo_api and the 3-package app (scratchpad deps-app), base vs
# head interleaved, a comment edit between runs, the same output path per (compiler, program),
# VELT_THREADS=1, median of 3 after a warm-up. Lazy std, or std from its unit on disk with
# `units` (each compiler its own unit dir). std and the runtime from <worktree>.
W=$1
B=$2
H=$3
MODE=${4:-lazy}
S=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad
D=$S/pr-ab
rm -rf "$D" && mkdir -p "$D/u-base" "$D/u-head" "$D/p"
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk VELT_STD=$W/std VELT_THREADS=1
export VELT_RT_LIB=$W/../d1-open-pr/target/debug/libvelt_rt.a
cp "$W/tests/golden/m1/hello.velt" "$D/p/hello.velt"
cp "$W/examples/todo_api.velt" "$D/p/todo_api.velt"
cp -R "$S/deps-app" "$D/deps"
rm -f "$D"/deps/o-* "$D"/deps/*.o
run() { # which velt cmd file
  local v=$1 cmd=$2 f=$3 tag E=()
  [ "$v" = "$B" ] && tag=base || tag=head
  [ "$MODE" = units ] && E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK="$D/u-$tag")
  echo "// $tag $cmd $RANDOM" >> "$f"
  local log
  case $cmd in
    check) log=$(cd "$(dirname "$f")" && env "${E[@]}" /usr/bin/time -l "$v" check "$f" 2>&1) ;;
    build) log=$(cd "$(dirname "$f")" && env "${E[@]}" /usr/bin/time -l "$v" build "$f" -o "$D/o-$tag-$(basename "$f" .velt)" 2>&1) ;;
  esac
  echo "$log" | grep -v '^velt' | grep -qE "^error|panicked|ICE" && echo "FAILED: $tag $cmd $f" >&2
  echo "$log" | awk '/instructions retired/ {print $1}'
}
for f in "$D/p/hello.velt" "$D/p/todo_api.velt" "$D/deps/main.velt"; do
  name=$(basename "$(dirname "$f")")/$(basename "$f")
  for cmd in check build; do
    run "$B" $cmd "$f" > /dev/null; run "$H" $cmd "$f" > /dev/null
    b=(); h=()
    for i in 1 2 3; do b+=("$(run "$B" $cmd "$f")"); h+=("$(run "$H" $cmd "$f")"); done
    mb=$(printf '%s\n' "${b[@]}" | sort -n | sed -n 2p)
    mh=$(printf '%s\n' "${h[@]}" | sort -n | sed -n 2p)
    printf '%-6s %-18s base %7.3f G  head %7.3f G  %+6.1f%%\n' "$cmd" "$name" \
      "$(echo "$mb / 1000000000" | bc -l)" "$(echo "$mh / 1000000000" | bc -l)" \
      "$(echo "($mh - $mb) * 100 / $mb" | bc -l)"
  done
done
