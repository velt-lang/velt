#!/usr/bin/env bash
# Cross-compiling check (#856), in two halves that run on different machines (release.yml):
#
#   scripts/cross.sh build <velt> <dist dir> <out dir>
#       Install every target pack in <dist dir> (velt-<v>-target-<triple>.tar.gz) other than this
#       machine's into the toolchain of <velt> (`velt target add --from`), then build hello and
#       examples/http_hello.vlt for each of those targets, debug and release, into
#       <out dir>/<triple>/.
#   scripts/cross.sh run <dir> <triple>...
#       Run what `build` made for each <triple> (found in <dir>/*/<triple>/: one directory per
#       build host): hello must print "Hello, Velt!", http_hello must answer a request.
#
# Bash on Linux, macOS and Windows (Git Bash); the HTTP request uses /dev/tcp.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)

get() { # GET / from port 8080, retrying while the server starts
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

case "${1:-}" in
build)
    velt=$2 dist=$3 out=$4
    host=$("$velt" --version | sed -n 's/.* \([^ ]*\))$/\1/p')
    [ -n "$host" ] || { echo "cannot read the host triple from \`velt --version\`" >&2; exit 1; }
    echo "host: $host"
    targets=()
    for pack in "$dist"/velt-*-target-*.tar.gz; do
        triple=${pack##*-target-}
        triple=${triple%.tar.gz}
        [ "$triple" = "$host" ] && continue
        "$velt" target add "$triple" --from "$pack"
        targets+=("$triple")
    done
    "$velt" target list
    for t in "${targets[@]}"; do
        ext=
        case "$t" in *windows*) ext=.exe ;; esac
        mkdir -p "$out/$t"
        for mode in debug release; do
            flag=
            [ "$mode" = release ] && flag=--release
            echo "== $t $mode"
            "$velt" build $flag --target "$t" "$repo/tests/golden/m1/hello.vlt" -o "$out/$t/hello-$mode$ext"
            "$velt" build $flag --target "$t" "$repo/examples/http_hello.vlt" -o "$out/$t/http-$mode$ext"
        done
        # Only the executables travel to the target's machine.
        find "$out/$t" -type f ! -name "*-debug$ext" ! -name "*-release$ext" -delete
    done
    ;;
run)
    dir=$2
    shift 2
    ran=0
    for t in "$@"; do
        for d in "$dir"/*/"$t"; do
            [ -d "$d" ] || continue
            for mode in debug release; do
                for exe in "$d"/hello-"$mode"*; do
                    chmod +x "$exe"
                    got=$("$exe" | tr -d '\r')
                    [ "$got" = "Hello, Velt!" ] || { echo "$exe printed '$got'" >&2; exit 1; }
                done
                for exe in "$d"/http-"$mode"*; do
                    chmod +x "$exe"
                    VELT_HELLO_SECONDS=3 "$exe" >/dev/null &
                    pid=$!
                    body=$(get || true)
                    wait "$pid"
                    case "$body" in
                        *"Hello, World!"*) ;;
                        *) echo "$exe answered '$body'" >&2; exit 1 ;;
                    esac
                done
                echo "ok: $d ($mode)"
                ran=$((ran + 1))
            done
        done
    done
    [ "$ran" -gt 0 ] || { echo "nothing to run for $* in $dir" >&2; exit 1; }
    ;;
*)
    echo "usage: scripts/cross.sh build <velt> <dist dir> <out dir> | run <dir> <triple>..." >&2
    exit 2
    ;;
esac
