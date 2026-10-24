#!/usr/bin/env bash
# Array views (#1485): what a loop over a view costs, and what a program with view types but
# no view pays.
#
#   bench/array_view/run.sh time <velt> [n]       release timings of loops over a view of n
#                                                 (default 10000) elements against a plain array
#   bench/array_view/run.sh instr <base velt> <head velt>
#                                                 cachegrind instruction counts (VELT_THREADS=1)
#                                                 of these programs (PROGRAMS="..." picks
#                                                 some), built by both compilers:
#                                                 no_view (a Map<string, string[]> loop with one
#                                                 guarded `any as string[]` that never runs) and
#                                                 sigx_like (string[] and JSXChild[] view types:
#                                                 `kids as JSXChild[]` of an `any` that holds a
#                                                 JSXChild[], so that the base compiler runs
#                                                 it), and push_local, forof_local, index_local
#                                                 and index_inc: loops over a local string[]
#                                                 with no_view's guarded conversion, and the
#                                                 loops described below (field_index ...)
#
# The programs are written to $WORK (default: a new temporary directory). Each compiler uses
# its own std (VELT_STD is left as each build finds it).
set -euo pipefail
mode=$1
shift
WORK=${WORK:-$(mktemp -d)}
mkdir -p "$WORK"

cat >"$WORK/loop.vlt" <<'EOF'
type Child = string | number | null;

function best(f: () => number, reps: number): number {
  let min = 1e18;
  for (let r = 0; r < reps; r++) {
    const t0 = performance.now();
    f();
    const t = performance.now() - t0;
    if (t < min) {
      min = t;
    }
  }
  return min;
}

function sumIndex(xs: Child[]): number {
  let s = 0;
  for (let i = 0; i < xs.length; i++) {
    const c = xs[i];
    if (typeof c === "number") {
      s += c;
    }
  }
  return s;
}

function sumForOf(xs: Child[]): number {
  let s = 0;
  for (const c of xs) {
    if (typeof c === "number") {
      s += c;
    }
  }
  return s;
}

function sumForEach(xs: Child[]): number {
  let s = 0;
  xs.forEach((c) => {
    if (typeof c === "number") {
      s += c;
    }
  });
  return s;
}

function main() {
  const n = Number(process.env.N ?? "10000");
  const raw: number[] = [];
  const plain: Child[] = [];
  for (let i = 0; i < n; i++) {
    raw.push(i);
    plain.push(i);
  }
  const view = raw as any as Child[];
  const reps = 20;
  const row = (what: string, f: (xs: Child[]) => number) => {
    const v = best(() => f(view), reps);
    const p = best(() => f(plain), reps);
    console.log(`${what}: view ${v.toFixed(3)} ms, plain ${p.toFixed(3)} ms`);
  };
  row("index", sumIndex);
  row("for...of", sumForOf);
  row("forEach", sumForEach);
}
EOF

cat >"$WORK/no_view.vlt" <<'EOF'
function main() {
  const m = new Map<string, string[]>();
  for (let i = 0; i < 200000; i++) {
    const k = "k" + (i % 997);
    const xs = m.get(k);
    if (xs === undefined) {
      m.set(k, ["v" + i]);
    } else {
      xs.push("v" + (i % 13));
    }
  }
  let total = 0;
  for (const [k, xs] of m) {
    total += k.length;
    for (const s of xs) {
      total += s.length;
    }
  }
  // One guarded conversion that never runs: `string[]` becomes a view type.
  const maybe: any = Date.now() < 0 ? ["x"] : null;
  if (maybe !== null) {
    const ys = maybe as string[];
    total += ys.length;
  }
  console.log(total);
}
EOF

cat >"$WORK/sigx_like.vlt" <<'EOF'
interface VNode {
  type: string;
  props: Record<string, string>;
  children: JSXChild[];
}
type JSXChild = VNode | string | number | boolean | null | undefined | JSXChild[];

function h(type: string, kids: any): VNode {
  const children = kids as JSXChild[];
  return { type, props: { class: "c" + type }, children };
}

function render(c: JSXChild, out: string[]) {
  if (c === null || c === undefined || c === false || c === true) {
    return;
  }
  if (typeof c === "string" || typeof c === "number") {
    out.push(String(c));
    return;
  }
  if (Array.isArray(c)) {
    for (const k of c) {
      render(k, out);
    }
    return;
  }
  const n = c as VNode;
  out.push(`<${n.type} class="${n.props.class}">`);
  for (const k of n.children) {
    render(k, out);
  }
  out.push(`</${n.type}>`);
}

function main() {
  let size = 0;
  for (let round = 0; round < 200; round++) {
    const rows: JSXChild[] = [];
    for (let i = 0; i < 50; i++) {
      const cells: JSXChild[] = ["cell", i, null, [String(round), "x"]];
      rows.push(h("td", cells));
    }
    const names: any = ["a", "b", "c"];
    const top: JSXChild[] = [h("tr", rows), (names as string[]).join(",")];
    const tree = h("table", top);
    const out: string[] = [];
    render(tree, out);
    size += out.join("").length;
  }
  console.log(size);
}
EOF

# Loops over a local `string[]` in a program with one guarded `any as string[]` that never runs
# (so `string[]` is a view type): a push, a `for...of`, an index loop and a `v[i++]` loop.
guard='  const maybe: any = Date.now() < 0 ? ["x"] : null;
  if (maybe !== null) {
    const ys = maybe as string[];
    total += ys.length;
  }'
fill='  const xs: string[] = [];
  for (let i = 0; i < 1000; i++) {
    xs.push("v" + (i % 10));
  }'
cat >"$WORK/push_local.vlt" <<EOF
function main() {
  let total = 0;
  const xs: string[] = [];
  for (let i = 0; i < 200000; i++) {
    xs.push("v");
  }
  total += xs.length;
$guard
  console.log(total);
}
EOF
cat >"$WORK/forof_local.vlt" <<EOF
function main() {
  let total = 0;
$fill
  for (let r = 0; r < 200; r++) {
    for (const s of xs) {
      total += s.length;
    }
  }
$guard
  console.log(total);
}
EOF
cat >"$WORK/index_local.vlt" <<EOF
function main() {
  let total = 0;
$fill
  for (let r = 0; r < 200; r++) {
    for (let i = 0; i < xs.length; i++) {
      total += xs[i].length;
    }
  }
$guard
  console.log(total);
}
EOF
cat >"$WORK/index_inc.vlt" <<EOF
function main() {
  let total = 0;
$fill
  for (let r = 0; r < 200; r++) {
    let i = 0;
    while (i < xs.length) {
      total += xs[i++].length;
    }
  }
$guard
  console.log(total);
}
EOF

# Loops that a local's versioning alone does not cover: an array in an object field, a loop
# that may reassign its array (never taken), the same for a push loop, a nested `rows[k][i]`,
# a `string[]` parameter of a function called 200 times, and an array in a module variable.
cat >"$WORK/field_index.vlt" <<EOF
function main() {
  let total = 0;
$fill
  const o = { xs };
  for (let r = 0; r < 200; r++) {
    for (let i = 0; i < o.xs.length; i++) {
      total += o.xs[i].length;
    }
  }
$guard
  console.log(total);
}
EOF
cat >"$WORK/maybe_assign.vlt" <<EOF
function main() {
  let total = 0;
  let xs: string[] = [];
  for (let i = 0; i < 1000; i++) {
    xs.push("v" + (i % 10));
  }
  for (let r = 0; r < 200; r++) {
    for (let i = 0; i < xs.length; i++) {
      total += xs[i].length;
      if (total < 0) {
        xs = [];
      }
    }
  }
$guard
  console.log(total);
}
EOF
cat >"$WORK/push_maybe.vlt" <<EOF
function main() {
  let total = 0;
  let xs: string[] = [];
  for (let i = 0; i < 200000; i++) {
    xs.push("v");
    if (i < 0) {
      xs = [];
    }
  }
  total += xs.length;
$guard
  console.log(total);
}
EOF
cat >"$WORK/rows_index.vlt" <<EOF
function main() {
  let total = 0;
  const rows: string[][] = [];
  for (let k = 0; k < 10; k++) {
    const row: string[] = [];
    for (let i = 0; i < 100; i++) {
      row.push("v" + (i % 10));
    }
    rows.push(row);
  }
  for (let r = 0; r < 200; r++) {
    for (let k = 0; k < rows.length; k++) {
      for (let i = 0; i < rows[k].length; i++) {
        total += rows[k][i].length;
      }
    }
  }
$guard
  console.log(total);
}
EOF
cat >"$WORK/sum_param.vlt" <<EOF
function sum(xs: string[]): number {
  let n = 0;
  for (let i = 0; i < xs.length; i++) {
    n += xs[i].length;
  }
  return n;
}

function main() {
  let total = 0;
$fill
  for (let r = 0; r < 200; r++) {
    total += sum(xs);
  }
$guard
  console.log(total);
}
EOF
cat >"$WORK/module_index.vlt" <<EOF
const gxs: string[] = [];

function main() {
  let total = 0;
  for (let i = 0; i < 1000; i++) {
    gxs.push("v" + (i % 10));
  }
  for (let r = 0; r < 200; r++) {
    for (let i = 0; i < gxs.length; i++) {
      total += gxs[i].length;
    }
  }
$guard
  console.log(total);
}
EOF

case $mode in
time)
  velt=$1
  n=${2:-10000}
  "$velt" build --release "$WORK/loop.vlt" -o "$WORK/loop" >/dev/null
  N=$n "$WORK/loop"
  ;;
instr)
  base_velt=$1
  head_velt=$2
  for p in ${PROGRAMS:-no_view sigx_like push_local forof_local index_local index_inc field_index maybe_assign push_maybe rows_index sum_param module_index}; do
    line="$p"
    for side in base head; do
      velt=$base_velt
      [ "$side" = head ] && velt=$head_velt
      "$velt" build --release "$WORK/$p.vlt" -o "$WORK/$p.$side" >/dev/null
      VELT_THREADS=1 valgrind --tool=cachegrind --cache-sim=no \
        --cachegrind-out-file="$WORK/$p.$side.cg" "$WORK/$p.$side" >/dev/null 2>"$WORK/$p.$side.log"
      n=$(grep -oE 'I +refs: +[0-9,]+' "$WORK/$p.$side.log" | grep -oE '[0-9,]+$' | tr -d ,)
      line="$line $side=$n"
      eval "$side=$n"
    done
    echo "$line change=$(awk -v b="$base" -v h="$head" 'BEGIN { printf "%+.2f%%", (h - b) * 100 / b }')"
  done
  ;;
*)
  echo "usage: $0 time <velt> [n] | instr <base velt> <head velt>" >&2
  exit 2
  ;;
esac
