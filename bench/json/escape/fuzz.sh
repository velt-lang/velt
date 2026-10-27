#!/usr/bin/env bash
# JSON.stringify of random strings (bench/json/escape/fuzz.vlt, valid TypeScript) under Node and
# under Velt: debug, --release, and on wasm32-wasip1 (wasmtime) debug and --release. Every
# output must be Node's, byte for byte. Prints one line per run and exits non-zero on a
# difference.
#
#   bench/json/escape/fuzz.sh [VELT]     (VELT defaults to `velt` on PATH)
#
# The runtime is the one next to VELT: run it with a debug and with a release `velt` to check
# both builds of the runtime. The wasm32 runs need the target's runtime (`cargo build -p
# velt_rt_wasm --target wasm32-wasip1`, with `--release` for a release `velt`) and wasmtime;
# they are skipped when wasmtime is missing.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
VELT=${1:-velt}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$HERE/fuzz.vlt" "$WORK/fuzz.ts"
cp "$HERE/fuzz.vlt" "$WORK/fuzz.vlt"
cd "$WORK"
node fuzz.ts > node.out
echo "node: $(wc -l < node.out) lines, sha256 $(sha256sum < node.out | cut -c1-16)"
modes=("debug|" "release|--release")
if command -v wasmtime > /dev/null; then
  modes+=("wasm32 debug|--target wasm32-wasip1" "wasm32 release|--target wasm32-wasip1 --release")
fi
status=0
for m in "${modes[@]}"; do
  name=${m%%|*}
  # shellcheck disable=SC2086 # the flags are words
  "$VELT" run ${m#*|} fuzz.vlt > out.txt
  if cmp -s node.out out.txt; then
    echo "$name: same"
  else
    echo "$name: DIFFERENT"
    status=1
  fi
done
exit $status
