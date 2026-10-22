#!/usr/bin/env bash
# Cost of `for...in` over a plain value in a program that makes a proxy (Linux, valgrind),
# built by the compiler of each git ref (release, LLVM):
#
# - q3_perf: a typed loop over a 5-field object; q3_noproxy: the same program without the proxy;
#   q4_any: the loop through `any`. Each runs at 2000 and at 22000 loops; the difference over
#   the 100000 extra keys gives the instructions per key, without start-up.
# - q5_many: 20000 objects of the type in an array and one `for...in` over the first, which
#   costs as much as the merge base's only while the type stays uncounted.
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
  for (let i = 0; i < LOOPS; i++) {
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
  for (let i = 0; i < LOOPS; i++) {
    for (const k in p) {
      s += k.length;
    }
  }
  return s;
}
console.log(run());
V
cat > "$W/q5_many.vlt" <<'V'
interface Props { a: number; b: number; c: number; d: number; e: number }
const mk = (): any => new Proxy({}, {});
mk();
function total(p: Props): number {
  return p.a + p.b + p.c + p.d + p.e;
}
function run(): number {
  const xs: Props[] = [];
  for (let i = 0; i < 20000; i++) {
    xs.push({ a: i, b: 2, c: 3, d: 4, e: 5 });
  }
  let s = 0;
  for (const p of xs) {
    s += total(p);
  }
  for (const k in xs[0]) {
    s += k.length;
  }
  return s;
}
console.log(run());
V
for prog in q3_perf q3_noproxy q4_any; do
  sed 's/LOOPS/2000/' "$W/$prog.vlt" > "$W/${prog}_2k.vlt"
  sed 's/LOOPS/22000/' "$W/$prog.vlt" > "$W/${prog}_22k.vlt"
done
# Instructions of the program `$1` built by `$2`.
count() {
  VELT_STD="$W/$2/std" "$W/bin-$2/velt" build --release --backend llvm "$W/$1.vlt" \
    -o "$W/$2-$1" >/dev/null 2>&1 || { echo "build failed: $2 $1" >&2; return; }
  valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null "$W/$2-$1" 2>&1 \
    >/dev/null | sed -n 's/.*I *refs: *//p' | tr -d ,
}
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
    a=$(count "${prog}_2k" "$n")
    b=$(count "${prog}_22k" "$n")
    per=$(awk -v a="$a" -v b="$b" 'BEGIN { printf "%.2f", (b - a) / 100000 }')
    echo "$n $prog Ir(2000)=$a Ir(22000)=$b per-key=$per"
  done
  echo "$n q5_many Ir=$(count q5_many "$n")"
done
