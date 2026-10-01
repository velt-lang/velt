#!/usr/bin/env bash
# Fills fuzz/corpus/<target>/ with seed inputs: every .vlt file in the repo for the source
# targets (plus the hand-written seeds in fuzz/seeds/<target>/), a few documents for the JSON
# reader. libFuzzer adds what it finds next to them.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$here/.."
for t in parse compile; do
    mkdir -p "$here/corpus/$t"
    find "$repo/tests/golden" "$repo/examples" "$repo/std" "$repo/tests/difftest/corpus" -name '*.vlt' \
        -exec cp {} "$here/corpus/$t/" \;
    if [ -d "$here/seeds/$t" ]; then
        cp "$here/seeds/$t"/*.vlt "$here/corpus/$t/"
    fi
done
mkdir -p "$here/corpus/json"
printf '%s' '{"a":[1,2.5,-0,1e400,true,null],"b":{"c":"xé\n\"q\""}}' > "$here/corpus/json/object"
printf '%s' '[[[]],{},"",0.1,-1e-7,12345678901234567890]' > "$here/corpus/json/nested"
printf '%s' '"😀 \u0000 \\ /"' > "$here/corpus/json/string"
echo "seeded $(ls "$here/corpus/parse" | wc -l | tr -d ' ') source files"
