// usage: see run.sh next to this file.
// Generates the any comparison matrix program (TS and Velt), 26 values x 26 values x 4 ops,
// written directly, in a closure, through fields, elements and calls.
const vals = [
  ["0", "number"], ["-0", "number"], ["1", "number"], ["NaN", "number"], ["1.5", "number"],
  ['""', "string"], ['"0"', "string"], ['"1"', "string"], ['"abc"', "string"], ['" "', "string"],
  ['"1.5"', "string"], ['"true"', "string"], ["true", "boolean"], ["false", "boolean"],
  ["null", "nil"], ["undefined", "nil"],
  ["inst", "object"], ["inst", "object"], ["other", "object"], ["subBase", "object"],
  ["arr", "object"], ["arr", "object"], ["empty", "object"], ["fn", "function"], ["fn", "function"],
  ["fn2", "function"],
];
const ops = ["===", "==", "!==", "!="];
const prim = (k) => k === "number" || k === "string" || k === "boolean";
// `==` between an object and a primitive stops with a TypeError in Velt (not supported yet).
const skip = (i, j, op) => (op === "==" || op === "!=") &&
  ((vals[i][1] === "object" && prim(vals[j][1])) || (vals[j][1] === "object" && prim(vals[i][1])));
const n = vals.length;
let out = [];
out.push(`class C { x: number = 1; }
class D extends C { y: number = 2; }
class H { v: any; constructor(v: any) { this.v = v; } }
function b(x: boolean): string { return x ? "1" : "0"; }

function at(t: any[], i: number): any { return t[i]; }
function main(): void {
  const inst = new C();
  const other = new C();
  const d = new D();
  const subBase: C = d;
  const arr: number[] = [1];
  const empty: number[] = [];
  const fn = (a: number): number => a + 1;
  const fn2 = (a: number): number => a + 1;`);
for (let i = 0; i < n; i++) out.push(`  const v${i}: any = ${vals[i][0]};`);
out.push(`  const table: any[] = [${vals.map((_, i) => "v" + i).join(", ")}];`);
out.push(`  const hs: H[] = [];\n  for (const t of table) { hs.push(new H(t)); }`);
out.push(`  const skip: boolean[][] = [${vals.map((_, i) => "[" + vals.map((_, j) => ops.map(op => skip(i, j, op) ? "true" : "false").join(", ")).join(", ") + "]").join(", ")}];`);
const direct = (pre) => {
  for (let i = 0; i < n; i++) {
    const parts = [];
    for (let j = 0; j < n; j++) for (const op of ops) parts.push(skip(i, j, op) ? '"-"' : `b(v${i} ${op} v${j})`);
    out.push(`${pre}console.log("${i}: " + ${parts.join(" + ")});`);
  }
};
out.push(`  console.log("direct");`);
direct("  ");
out.push(`  const inClosure = (): void => {\n    console.log("closure");`);
direct("    ");
out.push(`  };\n  inClosure();`);
const loop = (name, get) => {
  out.push(`  console.log("${name}");
  for (let i = 0; i < ${n}; i++) {
    let row = String(i) + ": ";
    for (let j = 0; j < ${n}; j++) {
      const k = skip[i][j * 4];
      row = row + (k ? "-" : b(${get("i")} === ${get("j")}));
      row = row + (skip[i][j * 4 + 1] ? "-" : b(${get("i")} == ${get("j")}));
      row = row + (skip[i][j * 4 + 2] ? "-" : b(${get("i")} !== ${get("j")}));
      row = row + (skip[i][j * 4 + 3] ? "-" : b(${get("i")} != ${get("j")}));
    }
    console.log(row);
  }`);
};
loop("fields", (v) => `hs[${v}].v`);
loop("elements", (v) => `table[${v}]`);
loop("calls", (v) => `at(table, ${v})`);
out.push("}");
const src = out.join("\n") + "\n";
require("fs").writeFileSync("matrix.vlt", src);
require("fs").writeFileSync("matrix.ts", src + "main();\n");
let skipped = 0; for (let i = 0; i < n; i++) for (let j = 0; j < n; j++) for (const op of ops) if (skip(i, j, op)) skipped++;
console.log("values", n, "skipped per form", skipped, "compared per form", n * n * 4 - skipped);
