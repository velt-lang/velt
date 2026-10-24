#!/usr/bin/env bash
# Array views (#1485): what a loop over a view costs, and what a program with view types but
# no view pays.
#
#   bench/array_view/run.sh time <velt> [n]       release timings of loops over a view of n
#                                                 (default 10000) elements against a plain array
#   bench/array_view/run.sh instr <base velt> <head velt>
#                                                 cachegrind instruction counts (VELT_THREADS=1)
#                                                 of two programs, built by both compilers:
#                                                 no_view (a Map<string, string[]> loop with one
#                                                 guarded `any as string[]` that never runs) and
#                                                 sigx_like (string[] and JSXChild[] view types:
#                                                 `kids as JSXChild[]` of an `any` that holds a
#                                                 JSXChild[], so that the base compiler runs it)
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
  for p in no_view sigx_like; do
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
