#!/usr/bin/env node
// Differential against Node for arguments held in a variable the callee assigns (#2480): every
// kind of value through every way of calling a closure or nested function, compared with Node's
// output, debug and release, with the debug allocator's checks on.
//
//   node scripts/closure-arg-diff.mjs <velt binary> [--form <substring>] [--kind <substring>] [--debug-only]
//
// One program per call form. Exits 1 on the first run that differs, after printing all of them.
import { mkdtempSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const argv = process.argv.slice(2);
const velt = argv[0];
const option = (name) => {
  const i = argv.indexOf(name);
  return i < 0 ? "" : argv[i + 1];
};
const debugOnly = argv.includes("--debug-only");
if (!velt) {
  console.error("usage: closure-arg-diff.mjs <velt binary> [--form s] [--kind s] [--debug-only]");
  process.exit(2);
}

const prelude = `
function heap(a: string): string { return a + "-".repeat(40); }
type P = { x: number };
type N = { x: number; n: string };
type W = { x: number; n: string };
class C { n: string; constructor(n: string) { this.n = n; } }
function touch(w: W) { w.x = w.x + 0; }
function opt<T>(x: T): T | null { return x; }
function und<T>(x: T): T | undefined { return x; }
`;

// A value of each kind: its type, what the variable starts with and is assigned, and text for a
// parameter or variable holding it. `byRef`: JavaScript holds it by reference, and the exclusivity
// rule keeps rejecting a call that reads it next to a change of the same variable.
const kinds = [
  { name: "string", t: "string", a: `heap("one")`, b: `heap("nine")`, show: (v) => v },
  { name: "literal", t: "string", a: `"one"`, b: `"nine"`, show: (v) => v },
  { name: "number", t: "number", a: "1", b: "9", show: (v) => `String(${v})` },
  { name: "boolean", t: "boolean", a: "true", b: "false", show: (v) => `String(${v})` },
  { name: "object", t: "P", a: "{ x: 1 }", b: "{ x: 9 }", show: (v) => `String(${v}.x)` },
  { name: "object-str", t: "N", a: `{ x: 1, n: heap("a") }`, b: `{ x: 9, n: heap("z") }`, show: (v) => `${v}.n + ${v}.x` },
  { name: "object-mut", t: "W", a: `{ x: 1, n: heap("a") }`, b: `{ x: 9, n: heap("z") }`, show: (v) => `${v}.n + ${v}.x` },
  { name: "union", t: "string | number", a: `heap("one")`, b: "9", show: (v) => `String(${v})` },
  { name: "optional", t: "string | null", a: `opt(heap("one"))`, b: "null", show: (v) => `String(${v})` },
  { name: "undefined", t: "string | undefined", a: `und(heap("one"))`, b: "undefined", show: (v) => `String(${v})` },
  { name: "optional-num", t: "number | null", a: "opt(1)", b: "null", show: (v) => `String(${v})` },
  { name: "optional-obj", t: "N | null", a: `opt<N>({ x: 1, n: heap("a") })`, b: "null", show: (v) => `(${v} === null ? "null" : ${v}.n)` },
  { name: "any", t: "any", a: `heap("one")`, b: "9", show: (v) => `String(${v})` },
  { name: "any-obj", t: "any", a: `{ x: 1 }`, b: `{ x: 9 }`, show: (v) => `String(${v}.x)` },
  { name: "tuple", t: "[number, string]", a: `[1, heap("a")]`, b: `[9, heap("z")]`, show: (v) => `${v}[0] + ${v}[1]` },
  { name: "array", t: "string[]", a: `[heap("a"), heap("b")]`, b: `[heap("z")]`, show: (v) => `${v}.join()`, byRef: true },
  { name: "class", t: "C", a: `new C(heap("a"))`, b: `new C(heap("z"))`, show: (v) => `${v}.n`, byRef: true },
];

// junk allocates over what a freed value's block held, so a parameter left pointing at it prints
// something else (or the debug allocator aborts).
const junk = `const junk = heap("xxxxxxxxxxxxxxxxxxxx"); `;

// A way of calling. `body(k, id)`: statements printing what Node prints. `top`: declared at module
// level (it cannot sit in a function). `noRef`: the form the exclusivity rule rejects for class
// instances and arrays.
const forms = [
  {
    name: "arrow",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = (v: ${k.t}) => { s = ${k.b}; ${junk}return ${k.show("v")}; };
  console.log("${id}", f(s), ${k.show("s")});`,
  },
  {
    name: "arrow-no-later-read",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = (v: ${k.t}) => { s = ${k.b}; ${junk}return ${k.show("v")}; };
  const r = f(s);
  console.log("${id}", r);`,
  },
  {
    name: "nested-function",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  function f(v: ${k.t}) { s = ${k.b}; ${junk}return ${k.show("v")}; }
  console.log("${id}", f(s), ${k.show("s")});`,
  },
  {
    name: "object-method",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const o = { go(v: ${k.t}) { s = ${k.b}; ${junk}return ${k.show("v")}; } };
  console.log("${id}", o.go(s), ${k.show("s")});`,
  },
  {
    name: "iife",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const r = ((v: ${k.t}) => { s = ${k.b}; ${junk}return ${k.show("v")}; })(s);
  console.log("${id}", r, ${k.show("s")});`,
  },
  {
    name: "field",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const h = { f: (v: ${k.t}) => { s = ${k.b}; ${junk}return ${k.show("v")}; } };
  console.log("${id}", h.f(s), ${k.show("s")});`,
  },
  {
    name: "element",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const fs = [(v: ${k.t}) => { s = ${k.b}; ${junk}return ${k.show("v")}; }];
  console.log("${id}", fs[0](s), ${k.show("s")});`,
  },
  {
    name: "callback-literal",
    noRef: true,
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const run = (v: ${k.t}, c: () => void) => { c(); ${junk}return ${k.show("v")}; };
  const r = run(s, () => { s = ${k.b}; });
  console.log("${id}", r, ${k.show("s")});`,
  },
  {
    name: "callback-held",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const c = (v: ${k.t}) => { s = ${k.b}; ${junk}return ${k.show("v")}; };
  const app = (g: (x: ${k.t}) => string, a: ${k.t}) => g(a);
  const r = app(c, s);
  console.log("${id}", r, ${k.show("s")});`,
  },
  {
    name: "returned",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const mk = () => (v: ${k.t}) => { s = ${k.b}; ${junk}return ${k.show("v")}; };
  console.log("${id}", mk()(s), ${k.show("s")});`,
  },
  {
    name: "default",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = (v: ${k.t}, w: string = "dflt") => { s = ${k.b}; ${junk}return ${k.show("v")} + w; };
  console.log("${id}", f(s), ${k.show("s")});`,
  },
  {
    name: "rest",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = (a: string, ...vs: ${k.t.includes("|") ? `(${k.t})` : k.t}[]) => { s = ${k.b}; ${junk}const v = vs[0]; const r = ${k.show("v")}; return a + r; };
  console.log("${id}", f("r", s), ${k.show("s")});`,
  },
  {
    name: "order",
    noRef: true,
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = (a: ${k.t}, b: ${k.t}) => ${k.show("a")} + "/" + ${k.show("b")};
  console.log("${id}", f(s, (s = ${k.b})), ${k.show("s")});`,
  },
  {
    name: "order-closure",
    noRef: true,
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = (a: ${k.t}, b: ${k.t}) => { s = ${k.a}; ${junk}return ${k.show("a")} + "/" + ${k.show("b")}; };
  console.log("${id}", f(s, (s = ${k.b})), ${k.show("s")});`,
  },
  {
    name: "loop",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = (v: ${k.t}) => { s = ${k.b}; ${junk}return ${k.show("v")}; };
  for (let i = 0; i < 3; i++) {
    if (i === 1) s = ${k.a};
    console.log("${id}", i, f(s), ${k.show("s")});
  }`,
  },
  {
    name: "loop-variable",
    body: (k, id) => `for (let i = 0, s: ${k.t} = ${k.a}; i < 2; i++) {
    const f = (v: ${k.t}) => { s = ${k.b}; ${junk}return ${k.show("v")}; };
    console.log("${id}", i, f(s), ${k.show("s")});
  }`,
  },
  {
    name: "module-let",
    top: (k, id) => `let s_${id}: ${k.t} = ${k.a};
const f_${id} = (v: ${k.t}) => { s_${id} = ${k.b}; ${junk}return ${k.show("v")}; };
function g_${id}(v: ${k.t}) { s_${id} = ${k.a}; ${junk}return ${k.show("v")}; }`,
    body: (k, id) => `console.log("${id}", f_${id}(s_${id}), ${k.show(`s_${id}`)});
  console.log("${id}", g_${id}(s_${id}), ${k.show(`s_${id}`)});`,
  },
  {
    name: "reads-both",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = (v: ${k.t}) => { const before = ${k.show("v")}; s = ${k.b}; ${junk}return before + "|" + ${k.show("v")} + "|" + ${k.show("s")}; };
  console.log("${id}", f(s), ${k.show("s")});`,
  },
  {
    name: "recursion",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = (v: ${k.t}, d: number): string => { if (d === 0) { s = ${k.b}; return ${k.show("v")}; } const r = f(s, d - 1); ${junk}return r + "," + ${k.show("v")}; };
  console.log("${id}", f(s, 2), ${k.show("s")});`,
  },
  {
    name: "async",
    async: true,
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = async (v: ${k.t}) => { s = ${k.b}; await tick(); ${junk}return ${k.show("v")}; };
  console.log("${id}", await f(s), ${k.show("s")});`,
  },
  {
    name: "async-after-await",
    async: true,
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = async (v: ${k.t}) => { await tick(); s = ${k.b}; await tick(); ${junk}return ${k.show("v")}; };
  const p = f(s);
  console.log("${id}", await p, ${k.show("s")});`,
  },
  {
    name: "async-caller-assigns",
    async: true,
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const f = async (v: ${k.t}) => { await tick(); ${junk}const r = ${k.show("v")}; return r; };
  const p = f(s);
  s = ${k.b};
  console.log("${id}", await p, ${k.show("s")});`,
  },
  {
    name: "generator",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  const g = function* (v: ${k.t}) { s = ${k.b}; ${junk}yield ${k.show("v")}; yield ${k.show("s")}; yield ${k.show("v")}; };
  console.log("${id}", [...g(s)].join(","), ${k.show("s")});`,
  },
  {
    name: "generator-caller-assigns",
    body: (k, id) => `let s: ${k.t} = ${k.a};
  function* g(v: ${k.t}) { yield ${k.show("v")}; s = ${k.b}; ${junk}yield ${k.show("v")}; }
  const it = g(s);
  s = ${k.a};
  console.log("${id}", [...it].join(","), ${k.show("s")});`,
  },
];

const wanted = (name, opt) => !opt || name.includes(opt);
const dir = mkdtempSync(join(tmpdir(), "closure-arg-diff-"));
const failures = [];
let runs = 0;
for (const form of forms.filter((f) => wanted(f.name, option("--form")))) {
  const used = kinds.filter((k) => wanted(k.name, option("--kind")) && !(form.noRef && k.byRef));
  if (used.length === 0) continue;
  let src = prelude + `async function tick() { await new Promise<void>((r) => setTimeout(() => r(), 0)); }\n`;
  const calls = [];
  for (const k of used) {
    const id = `${form.name}/${k.name}`.replace(/-/g, "_").replace("/", "__");
    if (form.top) src += form.top(k, id) + "\n";
    const fn = `t_${id}`;
    src += `${form.async ? "async " : ""}function ${fn}() {\n  ${form.body(k, id)}\n}\n`;
    calls.push(form.async ? `await ${fn}();` : `${fn}();`);
  }
  src += `async function all() {\n  ${calls.join("\n  ")}\n}\nall().then(() => console.log("done"));\n`;
  const base = join(dir, form.name.replace(/-/g, "_"));
  writeFileSync(base + ".velt", src);
  writeFileSync(base + ".ts", src);
  const node = spawnSync(process.execPath, [base + ".ts"], { encoding: "utf8" });
  if (node.status !== 0) {
    failures.push(`${form.name}: node failed:\n${node.stderr}`);
    continue;
  }
  for (const flags of debugOnly ? [[]] : [[], ["--release"]]) {
    const r = spawnSync(velt, ["run", ...flags, base + ".velt"], {
      encoding: "utf8",
      env: { ...process.env, VELT_RT_DEBUG_ALLOC: "1" },
    });
    runs++;
    const mode = flags.length ? "release" : "debug";
    if (r.status !== 0 || r.stdout !== node.stdout) {
      const want = node.stdout.split("\n");
      const got = r.stdout.split("\n");
      const diff = want.flatMap((l, i) => (l === got[i] ? [] : [`  node: ${l}\n  velt: ${got[i]}`]));
      failures.push(
        `${form.name} ${mode}: status ${r.status}\n${diff.slice(0, 40).join("\n")}\n${r.stderr.slice(0, 1500)}`,
      );
    }
  }
}
console.log(`${runs} runs, ${failures.length} differ (programs in ${dir})`);
for (const f of failures) console.log(f);
process.exit(failures.length ? 1 : 0);
