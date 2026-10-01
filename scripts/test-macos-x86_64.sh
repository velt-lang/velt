#!/usr/bin/env bash
# Test macOS x86_64 on an Apple Silicon Mac: cross-build every golden that has an `.out` with the
# native (arm64) `velt --target x86_64-apple-darwin`, in debug (Cranelift) and release (LLVM when
# clang is found), run the executables under Rosetta 2 and compare stdout and exit code like
# crates/veltc/tests/golden.rs does. Then runs the Cranelift JIT unit tests as an x86_64 process.
#
# Usage: scripts/test-macos-x86_64.sh [<filter>]   # filter: substring of the golden path
# Prerequisites: Rosetta 2 (`softwareupdate --install-rosetta`); the script adds the rustup target.
set -euo pipefail

filter=${1:-}
target=x86_64-apple-darwin
cd "$(dirname "$0")/.."
root=$(pwd)

if [ "$(uname -sm)" != "Darwin arm64" ]; then
    echo "error: this script runs on an Apple Silicon Mac" >&2
    exit 2
fi
if ! arch -x86_64 /usr/bin/true 2>/dev/null; then
    echo "error: Rosetta 2 is not installed; run \`softwareupdate --install-rosetta\`" >&2
    exit 1
fi

step() {
    printf '\033[36m==> %s\033[0m\n' "$*"
}

step "rustup target add $target"
rustup target add "$target" >/dev/null
step "build velt (host) and the x86_64 runtime"
cargo build -q -p veltc
cargo build -q -p velt_rt --target "$target"
export VELT_RT_LIB="$root/target/$target/debug/libvelt_rt.a"
velt="$root/target/debug/velt"
work="$root/target/golden-x86_64"
mkdir -p "$work"

goldens() {
    find tests/golden -name '*.vlt' ! -name '_*' | sort
    for f in examples/*.vlt; do
        [ -f "${f%.vlt}.out" ] && echo "$f"
    done
}

# A golden is pending when a directory between it and tests/golden holds a `.pending` file.
pending() {
    local d
    d=$(dirname "$1")
    while [ "$d" != "tests" ] && [ "$d" != "." ]; do
        [ -f "$d/.pending" ] && return 0
        d=$(dirname "$d")
    done
    return 1
}

failures=0
checked=0
check() {
    local f=$1 mode=$2
    local exe="$work/$(echo "${f%.vlt}" | tr / _)_$mode"
    local flags=()
    [ "$mode" = release ] && flags=(--release)
    if ! (cd "$work" && "$velt" build ${flags[@]+"${flags[@]}"} --target "$target" -o "$exe" "$root/$f") >"$exe.log" 2>&1; then
        echo "FAIL $f [$mode]: build failed"
        cat "$exe.log"
        return 1
    fi
    if ! file "$exe" | grep -q x86_64; then
        echo "FAIL $f [$mode]: not an x86_64 executable: $(file "$exe")"
        return 1
    fi
    local want_code=0 code=0
    [ -f "${f%.vlt}.code" ] && want_code=$(tr -d '[:space:]' <"${f%.vlt}.code")
    (cd "$work" && arch -x86_64 "$exe") >"$exe.stdout" 2>"$exe.stderr" || code=$?
    if [ "$code" != "$want_code" ] || ! diff <(tr -d '\r' <"${f%.vlt}.out") <(tr -d '\r' <"$exe.stdout") >"$exe.diff"; then
        echo "FAIL $f [$mode]: exit $code (want $want_code)"
        cat "$exe.diff" "$exe.stderr"
        return 1
    fi
}

step "goldens: build --target $target, run under Rosetta"
while read -r f; do
    [ -f "${f%.vlt}.out" ] || continue
    case "$f" in *"$filter"*) ;; *) continue ;; esac
    pending "$f" && continue
    for mode in debug release; do
        checked=$((checked + 1))
        check "$f" "$mode" || failures=$((failures + 1))
    done
done < <(goldens)
echo "x86_64 goldens: $checked runs, $failures failures"

step "cargo test -p velt_codegen_cl --target $target (JIT tests run as x86_64)"
cargo test -q -p velt_codegen_cl --target "$target"

[ "$failures" = 0 ] || exit 1
printf '\033[32mall macOS x86_64 checks passed (%s)\033[0m\n' "$(git rev-parse --short HEAD)"
