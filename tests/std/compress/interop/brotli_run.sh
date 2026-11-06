#!/bin/sh
# Compresses the corpus with velt/compress's Brotli encoder and has Node's decoder read every
# stream, then prints the sizes against Node's encoder. Needs `velt` and `node`; not part of the
# test suite, which has no Node to rely on. Usage: brotli_run.sh [work directory].
set -eu
here=$(cd "$(dirname "$0")" && pwd)
work=${1:-$(mktemp -d)}
mkdir -p "$work/corpus" "$work/brotli"
node "$here/corpus.mjs" "$work/corpus"
${VELT:-velt} build --release "$here/brotli_encode.velt" -o "$work/brotli_encode"
"$work/brotli_encode" "$work/corpus" "$work/brotli"
node "$here/brotli_check.mjs" "$work/corpus" "$work/brotli"
