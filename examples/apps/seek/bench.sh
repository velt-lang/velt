#!/usr/bin/env bash
# seek vs ripgrep vs git grep on this repository (warm cache, hyperfine). Checks first that seek
# and ripgrep print the same lines for each pattern.
#   ./bench.sh [runs]        (default 20; needs hyperfine, rg and git; RG=/path/to/rg to pick one)
set -euo pipefail
cd "$(dirname "$0")"
runs=${1:-20}
# A release velt: a debug one links the debug runtime, which is several times slower.
velt=${VELT:-../../../target/release/velt}
[ -x "$velt" ] || velt=velt
rg=${RG:-rg}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
"$velt" build --release -o "$tmp/seek" >/dev/null
repo=$(cd ../../.. && pwd)
cd "$repo"

same() { # same <rg/seek args...>: both print the same lines
    if ! diff <("$rg" -n --no-heading --color never "$@" . </dev/null | sed 's#^\./##' | sort) \
        <("$tmp/seek" --color never "$@" | sort) >/dev/null; then
        echo "seek and rg differ on: $*" >&2
        exit 1
    fi
}
same unsafe
same -i velt_rt_str
same 'fn \w+_index_of'
echo "seek and rg print the same lines"

hyperfine -N --warmup 3 --runs "$runs" --style basic --export-markdown "$tmp/results.md" \
    -n "seek literal" "$tmp/seek --color never unsafe" \
    -n "rg literal" "$rg -n --no-heading --color never unsafe ." \
    -n "git grep literal" "git grep -n unsafe" \
    -n "seek regex" "$tmp/seek --color never 'fn \\w+_(index|slice)'" \
    -n "rg regex" "$rg -n --no-heading --color never 'fn \\w+_(index|slice)' ." \
    -n "seek -i regex" "$tmp/seek --color never -i 'velt_rt_[a-z]+_new'" \
    -n "rg -i regex" "$rg -n --no-heading --color never -i 'velt_rt_[a-z]+_new' ." \
    -n "seek --files" "$tmp/seek --files --color never" \
    -n "rg --files" "$rg --files ." >/dev/null
echo
echo "$(git ls-files | wc -l | tr -d ' ') tracked files; $(nproc 2>/dev/null || sysctl -n hw.ncpu) cores"
cat "$tmp/results.md"
