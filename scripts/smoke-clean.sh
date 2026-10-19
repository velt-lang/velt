#!/usr/bin/env bash
# Smoke test of an installed toolchain on a machine without a C toolchain (no cc, ld or Xcode):
# with the bundled linker forced ($VELT_LINKER=bundled), `velt doctor` reports it, and hello and
# examples/http_hello.vlt build in debug and release mode, run and answer a request. Extra
# arguments are more targets to build and run the same way (e.g. x86_64-unknown-linux-musl).
# release.yml runs it in debian:bookworm-slim and on macOS; scripts/smoke-clean.ps1 is the
# Windows counterpart. Needs bash (for /dev/tcp: slim images have no curl).
#
# Usage: scripts/smoke-clean.sh <path to velt> [<target>...]
set -euo pipefail

velt=$1
shift
repo=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export VELT_LINKER=bundled

step() { printf '\n== %s\n' "$*"; }

# GET / from the server on port 8080, retrying while it starts.
get() {
    for _ in $(seq 50); do
        if { exec 3<>/dev/tcp/127.0.0.1/8080; } 2>/dev/null; then
            printf 'GET / HTTP/1.0\r\nHost: localhost\r\n\r\n' >&3
            cat <&3
            exec 3>&-
            return 0
        fi
        sleep 0.1
    done
    return 1
}

run_both() { # <label> [velt build flags...]
    local label=$1
    shift
    "$velt" build "$@" "$repo/tests/golden/m1/hello.vlt" -o "$work/hello"
    out=$("$work/hello")
    [ "$out" = "Hello, Velt!" ] || { echo "$label hello printed '$out'" >&2; exit 1; }
    "$velt" build "$@" "$repo/examples/http_hello.vlt" -o "$work/http"
    VELT_HELLO_SECONDS=3 "$work/http" >/dev/null &
    pid=$!
    body=$(get || true)
    wait "$pid"
    case "$body" in
        *"Hello, World!"*) echo "ok: $label" ;;
        *) echo "$label http_hello answered '$body'" >&2; exit 1 ;;
    esac
}

step "no C toolchain"
for tool in cc ld; do
    if command -v "$tool" >/dev/null 2>&1; then echo "note: $tool is on PATH ($(command -v "$tool")); \$VELT_LINKER=bundled keeps it unused"; fi
done
step "velt doctor"
"$velt" doctor | tee "$work/doctor"
grep -q 'linker .*bundled' "$work/doctor" || { echo "velt doctor does not report the bundled linker" >&2; exit 1; }
step "debug build (shared runtime)"
run_both debug
step "release build"
run_both release --release
for target in "$@"; do
    step "--target $target (debug and release)"
    run_both "$target debug" --target "$target"
    run_both "$target release" --target "$target" --release
done
