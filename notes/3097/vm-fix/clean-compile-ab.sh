#!/bin/bash
# Deletes what compile-ab.sh and irsize.sh left in $HOME/b3097fix (main's velt build and sources,
# the measured outputs); keeps tgt, tgt-units and the std exports the counts use.
set -u
OUT=$HOME/b3097fix
rm -rf "$OUT/tgt-main" "$OUT/src-main" "$OUT"/sz-* "$OUT"/ir-* "$OUT"/cold-* "$OUT"/actors-* "$OUT"/std-nogather "$OUT"/std-nospin
du -sh "$OUT"
