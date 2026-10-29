// Compares the prototype tables `key in x` through `any` answers from
// (crates/velt_vir/src/lower/any_has_builtin.rs and crates/velt_sema/src/js_protos.rs, whose
// tables also say which reads give `undefined`) with what the
// running Node's built-ins have: `Reflect.ownKeys` of each prototype (symbols as the key names
// sema gives them, `[Symbol.iterator]`), with `Object.prototype`'s names left out.
//
//   node scripts/any-has-tables.mjs
//
// A name Node has that a table lacks is a difference on every Node. A name a table has that this
// Node lacks is one only on the Node the tables follow (REFERENCE_MAJOR or newer): an older Node
// lacks the newest methods (`Map.prototype.getOrInsert`). Exits 1 on a difference.
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const REFERENCE_MAJOR = 26;
const crates = join(dirname(fileURLToPath(import.meta.url)), "../crates");

/** The `const NAME: &[&str] = &[...]` (or `[&str; N] = [...]`) tables of a Rust file. */
function tables(file) {
  const src = readFileSync(join(crates, file), "utf8");
  const out = new Map();
  const re = /const (\w+): (?:&\[&str\]|\[&str; \d+\]) = &?\[([^\]]*(?:\][^;\]]*)*?)\];/g;
  for (const m of src.matchAll(re)) {
    out.set(m[1], [...m[2].matchAll(/"([^"]*)"/g)].map((x) => x[1]));
  }
  return out;
}

const key = (k) => (typeof k === "symbol" ? `[${k.description}]` : k);
const own = (o) => Reflect.ownKeys(o).map(key);
const objectNames = own(Object.prototype);
const typedArray = Object.getPrototypeOf(Int8Array.prototype);
const minus = (a, b) => a.filter((x) => !b.includes(x));

// What each table stands for, besides `Object.prototype`'s names.
const expected = {
  OBJECT_PROTO: objectNames,
  ARRAY_PROTO: minus(own(Array.prototype), ["length"]),
  TYPED_ARRAY_PROTO: [...own(typedArray), ...own(Int8Array.prototype)],
  UINT8_ARRAY_PROTO: minus(own(Uint8Array.prototype), own(Int8Array.prototype)),
  MAP_PROTO: own(Map.prototype),
  SET_PROTO: own(Set.prototype),
  WEAK_MAP_PROTO: own(WeakMap.prototype),
  WEAK_SET_PROTO: own(WeakSet.prototype),
  DATE_PROTO: own(Date.prototype),
  REGEXP_PROTO: [...own(RegExp.prototype), ...own(/a/)],
  ERROR_PROTO: [...own(Error.prototype), ...own(new Error("x"))],
  PROMISE_PROTO: own(Promise.prototype),
  ARRAY_BUFFER_PROTO: own(ArrayBuffer.prototype),
  DATA_VIEW_PROTO: own(DataView.prototype),
  FN_PROTO_NAMES: own(Function.prototype),
  STRING_PROTO: minus(own(String.prototype), ["length"]),
  NUMBER_PROTO: own(Number.prototype),
};

const have = new Map(
  ["velt_vir/src/lower/any_has_builtin.rs", "velt_sema/src/js_protos.rs"].flatMap((f) => [
    ...tables(f),
  ]),
);
const reference = Number(process.versions.node.split(".")[0]) >= REFERENCE_MAJOR;
let bad = 0;
for (const [name, want] of Object.entries(expected)) {
  const got = have.get(name);
  if (!got) {
    console.log(`${name}: no such table`);
    bad++;
    continue;
  }
  const w = name === "OBJECT_PROTO" ? want : minus(want, objectNames);
  const missing = minus(w, got);
  const extra = minus(got, w);
  if (missing.length) {
    console.log(`${name}: Node ${process.versions.node} has ${missing.join(", ")}`);
    bad++;
  }
  if (extra.length) {
    console.log(`${name}: Node ${process.versions.node} lacks ${extra.join(", ")}`);
    bad += reference ? 1 : 0;
  }
}
console.log(bad ? `${bad} difference(s)` : `the tables match Node ${process.versions.node} (they follow Node ${REFERENCE_MAJOR})`);
process.exit(bad ? 1 : 0);
