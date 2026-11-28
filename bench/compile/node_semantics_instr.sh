#!/usr/bin/env bash
# Instructions (cachegrind, one thread) for #3277, #3108 and #3118: two compilers checking and
# lowering to VIR (`build --emit=vir`) hello and a generated 10,000-line application
# (scale/app.py), and the programs each builds (release) running: pidigits on BigInt
# (`--release`, 600 digits), a BigInt loop of compound assignments, Promise.all over object
# literals' async methods and converted promises, and printing JsonValues.
# Linux only (valgrind).
#
#   bench/compile/node_semantics_instr.sh BASE_TREE HEAD_TREE [WORK]
#
# Each tree is a checkout with its release compiler and runtime built (`cargo build --release
# -p veltc --bin velt -p velt_rt`, in <tree>/target); each side reads its own std. Prints
# `<program> <check|vir|run> base=<n> head=<n> <change>`.
set -uo pipefail
base=$(cd "$1" && pwd) head=$(cd "$2" && pwd)
work=${3:-$(mktemp -d)}
mkdir -p "$work"
here=$(cd "$(dirname "$0")" && pwd)
printf 'console.log("hello");\n' > "$work/hello.velt"
python3 "$here/scale/app.py" 10000 "$work/app10k" > /dev/null
cp "$here/../benchmarks-game/pidigits/main.velt" "$work/pidigits.velt"
cat > "$work/bigint_ops.velt" <<'EOF'
import { BigInt } from "velt/math";
let acc = new BigInt(1);
const step = new BigInt(3);
for (let i = 0; i < 20000; i++) {
  acc += step;
  acc *= step;
  acc %= new BigInt(1000000007);
}
console.log(acc.toString());
EOF
cat > "$work/promises.velt" <<'EOF'
type Res<O> = { value: O };
interface Schema<O> {
  validate(value: unknown): Promise<Res<O>>;
}
const count: Schema<number> = {
  async validate(value: unknown): Promise<Res<number>> {
    if (typeof value === "number") {
      return { value: value + 1 };
    }
    throw new Error(String(value));
  },
};
async function main(): Promise<void> {
  let s = 0;
  for (let i = 0; i < 3000; i++) {
    const a: Promise<unknown> = count.validate(i).then((r) => r.value);
    const rs = await Promise.all([count.validate(i), count.validate(i + 1)]);
    s += rs[0].value + rs[1].value;
    await a;
  }
  console.log(s);
}
main();
EOF
cat > "$work/json_print.velt" <<'EOF'
const v = JsonValue.parse('{"a":[1,-2.5,"x",null,{"b":[3,4]}],"n":0,"s":"t"}');
let n = 0;
for (let i = 0; i < 2000; i++) {
  v.set("i", JsonValue.of(i));
  const s = `${v.get("a")!}|${v}`;
  n += s.length;
  if (i % 500 == 0) {
    console.log(v);
  }
}
console.log(n);
EOF
ir() { # <tree> <dir> <check|vir> <file>
  local cmd=(check "$4")
  [ "$3" = vir ] && cmd=(build --emit=vir "$4")
  (cd "$2" && VELT_THREADS=1 VELT_BUILD_CACHE=off VELT_STD="$1/std" valgrind --tool=cachegrind \
    --cache-sim=no --cachegrind-out-file=/dev/null "$1/target/release/velt" "${cmd[@]}" 2>&1 >/dev/null |
    sed -n 's/.*I *refs: *//p' | tr -d ,)
}
run() { # <tree> <file> <args...>
  local exe="$work/$(basename "$1")-$(basename "$2" .velt)"
  VELT_BUILD_CACHE=off VELT_STD="$1/std" "$1/target/release/velt" build --release "$work/$2" \
    -o "$exe" > /dev/null 2>&1 || { echo "build failed"; return; }
  shift 2
  VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no --cachegrind-out-file=/dev/null \
    "$exe" "$@" 2>&1 >/dev/null | sed -n 's/.*I *refs: *//p' | tr -d ,
}
row() { # <name> <mode> <base count> <head count>
  awk -v n="$1" -v m="$2" -v b="$3" -v h="$4" \
    'BEGIN { printf "%-11s %-6s base=%s head=%s %+.3f%%\n", n, m, b, h, 100 * (h - b) / b }'
}
for spec in hello::hello.velt:check hello::hello.velt:vir app10k:app10k:main.velt:check \
            app10k:app10k:main.velt:vir; do
  IFS=: read -r name dir file mode <<< "$spec"
  row "$name" "$mode" "$(ir "$base" "$work/$dir" "$mode" "$file")" \
    "$(ir "$head" "$work/$dir" "$mode" "$file")"
done
row pidigits run "$(run "$base" pidigits.velt 600)" "$(run "$head" pidigits.velt 600)"
for p in bigint_ops promises json_print; do
  row "$p" run "$(run "$base" "$p.velt")" "$(run "$head" "$p.velt")"
done
