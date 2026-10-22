#!/usr/bin/env bash
# Cost of `for...in` over a plain value in a program that makes a proxy (Linux, valgrind):
# instruction counts of a typed loop over a 5-field object (q3_perf), the same program without
# the proxy (q3_noproxy) and the loop through `any` (q4_any), 2000 loops of 5 keys each, built
# by the compiler of each git ref (release, LLVM).
#
#   bench/proxy/forin_cost.sh WORK_DIR REF:NAME...
#
# Run it from inside the repository, or with GIT_DIR set.
set -uo pipefail
W=$1
shift
mkdir -p "$W"
cat > "$W/q3_perf.vlt" <<'V'
interface Props { a: number; b: number; c: number; d: number; e: number }
const mk = (): any => new Proxy({}, {});
mk();
function run(): number {
  const p: Props = { a: 1, b: 2, c: 3, d: 4, e: 5 };
  let s = 0;
  for (let i = 0; i < 2000; i++) {
    for (const k in p) {
      s += k.length;
    }
  }
  return s;
}
console.log(run());
V
sed 's/^mk();$//; s/^const mk.*$//' "$W/q3_perf.vlt" > "$W/q3_noproxy.vlt"
cat > "$W/q4_any.vlt" <<'V'
const mk = (): any => new Proxy({}, {});
mk();
function run(): number {
  const p: any = { a: 1, b: 2, c: 3, d: 4, e: 5 };
  let s = 0;
  for (let i = 0; i < 2000; i++) {
    for (const k in p) {
      s += k.length;
    }
  }
  return s;
}
console.log(run());
V
for r in "$@"; do
  ref=${r%%:*}
  n=${r##*:}
  if [ ! -x "$W/bin-$n/velt" ]; then
    rm -rf "$W/$n"
    mkdir -p "$W/$n" "$W/bin-$n"
    git archive "$ref" | tar -x -C "$W/$n"
    # The archive's files carry the commit's time: newer ones make cargo rebuild in the shared
    # target directory.
    find "$W/$n" -type f -exec touch {} +
    (cd "$W/$n" && CARGO_TARGET_DIR="$W/target" cargo build --release -q -p veltc -p velt_rt) ||
      echo "build $n failed"
    cp "$W/target/release/velt" "$W/target/release/libvelt_rt.a" "$W/bin-$n/"
  fi
  for prog in q3_perf q3_noproxy q4_any; do
    VELT_STD="$W/$n/std" "$W/bin-$n/velt" build --release --backend llvm "$W/$prog.vlt" \
      -o "$W/$n-$prog" >/dev/null 2>&1 || echo "build failed: $n $prog"
    out=$("$W/$n-$prog")
    ir=$(valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
      "$W/$n-$prog" 2>&1 >/dev/null | sed -n 's/.*I *refs: *//p' | tr -d ,)
    echo "$n $prog out=$out Ir=$ir"
  done
done
