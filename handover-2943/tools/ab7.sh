#!/bin/bash
# ab7.sh: warm `velt check` and `velt build` instructions after a one-line edit, lazy std vs
# units from disk, for hello and todo_api; median of 3 after one warm-up run; flags failures.
# Works on a scratch copy of the programs (the edit appends lines to the file).
R=/Users/andii/dev/velt/branches/d1-open-pr
# The compiler to measure: $1, or the worktree's debug build.
V=${1:-$R/target/debug/velt}
B=/private/tmp/claude-501/-Users-andii-dev-velt-main/7de780fb-b302-4be3-ab45-b11b64221458/scratchpad/ab
S=$B/examples
rm -rf "${B:?}" && mkdir -p "$S" "$B/packages"
cp -R "$R/examples/." "$S/"
# examples/package.velt depends on ../packages/http-server.
cp -R "$R/packages/http-server" "$B/packages/"
rm -rf "$S/apps"
cp "$R/tests/golden/m1/hello.velt" "$S/hello.velt"
export VELT_STD=$R/std VELT_RT_LIB=$R/target/debug/libvelt_rt.a VELT_THREADS=1
export SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk
cd "$S" || exit 1
for cmd in check build; do
  for p in hello todo_api; do
    for mode in lazy units; do
      case $mode in
        lazy) E=() ;;
        units) E=(VELT_UNITS=2 VELT_UNITS_SPLIT=1 VELT_UNITS_BUILD=1 VELT_UNITS_DISK=auto) ;;
      esac
      vals=()
      for i in 1 2 3 4; do
        echo "const z$RANDOM$i = 1;" >> "$p.velt"
        o="$S/$p-$mode-$i-$RANDOM.out"
        case $cmd in
          check) log=$(env "${E[@]}" /usr/bin/time -l "$V" check "$p.velt" 2>&1) ;;
          build) log=$(env "${E[@]}" /usr/bin/time -l "$V" build "$p.velt" -o "$o" 2>&1 >/dev/null) ;;
        esac
        bad=0
        echo "$log" | grep -v '^velt' | grep -qE "error|panicked|ICE" && bad=1
        [ "$cmd" = build ] && [ ! -x "$o" ] && bad=1
        [ "$bad" = 1 ] && echo "FAILED: $cmd $p $mode run $i" >&2
        rm -f "$o"
        [ "$i" = 1 ] && continue
        vals+=("$(echo "$log" | awk '/instructions retired/ {print $1}')")
      done
      med=$(printf '%s\n' "${vals[@]}" | sort -n | sed -n 2p)
      printf '%-6s %-9s %-6s %6.2f G\n' "$cmd" "$p" "$mode" "$(echo "$med / 1000000000" | bc -l)"
    done
  done
done
