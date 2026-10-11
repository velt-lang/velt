#!/usr/bin/env bash
# Times of symbol operations (review of #968): 1e7 comparisons with a `Symbol.for` module
# constant against the same loop with a `Symbol()` constant, and the memory 1e7 `Symbol()`
# calls at run time keep (each record lives until the program ends).
#
#   bench/symbols.sh <velt>
set -euo pipefail
velt=$1
tmp=$(mktemp -d)
loop() {
  cat <<EOF
const R = $1;
function main(): void {
  const syms = [Symbol("a"), Symbol.for("r"), Symbol("c")];
  let n = 0;
  const t0 = Date.now();
  for (let i = 0; i < 10000000; i++) {
    if (syms[i % 3] === R) n++;
  }
  console.log("$2", n, "ms", Date.now() - t0);
}
EOF
}
loop 'Symbol.for("r")' 'Symbol.for' >"$tmp/for.vlt"
loop 'Symbol("r")' 'Symbol' >"$tmp/own.vlt"
cat >"$tmp/fresh.vlt" <<'EOF'
function main(): void {
  let n = 0;
  for (let i = 0; i < 10000000; i++) {
    const s = Symbol("x");
    if (s.description !== null) n++;
  }
  console.log("Symbol() x1e7", n, "rss MB", Math.round((process.memoryUsage().rss as number) / 1048576));
}
EOF
for p in for own fresh; do
  "$velt" build --release "$tmp/$p.vlt" -o "$tmp/$p.exe" >/dev/null
done
for _ in 1 2 3 4 5; do
  "$tmp/for.exe"
  "$tmp/own.exe"
done
"$tmp/fresh.exe"
rm -rf "$tmp"
