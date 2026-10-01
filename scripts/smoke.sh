#!/usr/bin/env sh
# End-to-end smoke test of a built `velt` on Linux/macOS: `velt doctor`, `velt new` + `run`
# (debug and release), and an HTTP request against examples/http_hello.vlt (release).
# Shared by scripts/test-linux.sh and scripts/check-all.sh.
#
# Usage: scripts/smoke.sh [<path to velt>]   (default: target/debug/velt of this checkout)
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
velt=${1:-"$repo/target/debug/velt"}
# Absolute, because the `velt new` step runs from a temporary directory.
velt="$(cd "$(dirname "$velt")" && pwd)/$(basename "$velt")"

step() {
    printf '\n== %s\n' "$*"
}

http_smoke() {
    if ! command -v curl >/dev/null 2>&1; then
        echo "curl not installed; skipping"
        return 0
    fi
    bin="$repo/target/smoke/http_hello"
    "$velt" build --release "$repo/examples/http_hello.vlt" -o "$bin"
    VELT_HELLO_SECONDS=3 "$bin" &
    pid=$!
    i=0
    until curl -fs -o /dev/null http://127.0.0.1:8080/ || [ $i -ge 50 ]; do
        sleep 0.1
        i=$((i + 1))
    done
    body=$(curl -fs http://127.0.0.1:8080/ || true)
    wait "$pid"
    [ "$body" = "Hello, World!" ] || { echo "unexpected response: '$body'" >&2; return 1; }
    echo "ok: $body"
}

step "velt doctor"
"$velt" doctor
step "velt new + run (debug and release)"
tmp=$(mktemp -d)
(cd "$tmp" && "$velt" new demo >/dev/null && cd demo && "$velt" run && "$velt" run --release)
rm -rf "$tmp"
step "HTTP smoke test (examples/http_hello.vlt, release)"
http_smoke
