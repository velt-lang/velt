#!/bin/sh
# Compresses a corpus with velt/compress and has Node's zlib and gzip read every stream, then
# prints the sizes against Node's. Needs `velt`, `node` and `gzip`; not part of the test suite,
# which has neither of the last two to rely on. Usage: run.sh [work directory].
set -eu
here=$(cd "$(dirname "$0")" && pwd)
work=${1:-$(mktemp -d)}
mkdir -p "$work/corpus" "$work/encoded"
node "$here/corpus.mjs" "$work/corpus"
${VELT:-velt} build --release "$here/encode.velt" -o "$work/encode"
"$work/encode" "$work/corpus" "$work/encoded"
node "$here/check.mjs" "$work/corpus" "$work/encoded"
