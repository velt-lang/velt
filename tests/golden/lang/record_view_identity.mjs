// Node twin of record_view_identity.velt: the casts are no-ops in TypeScript.
class Point {
  x = 1;
  y = 2;
}

function sameBag(bag, o) {
  return bag === o;
}

function toAny(v) {
  return v;
}

function row(label, o) {
  const r = o;
  const m = new Map();
  m.set(o, 7);
  const s = new Set();
  s.add(r);
  const back = r;
  console.log(
    `${label}: === ${r === o} !== ${r !== o} is ${Object.is(r, o)} map ${m.get(r) ?? 0} ` +
      `set ${s.has(o)} callee ${sameBag(r, o)} back ${back === r}`,
  );
}

row("literal", { a: 1, b: "x" });
row("class", new Point());
row("array", [1, 2, 3]);
row("map", new Map());
const o = { k: 1 };
const r1 = o;
const r2 = o;
console.log(`two views: ${r1 === r2} ${Object.is(r1, r2)}`);
r1["k"] = 5;
console.log(`write through the view: ${o.k}`);
const other = { k: 1 };
console.log(`other object: ${other === o}`);
const pt = new Point();
console.log(`last reference: ${toAny(pt) === pt}`);
const p = new Proxy({ a: 1 }, {});
const pr = p;
const pa = pr;
console.log(`proxy: ${pa === p} ${pa.a}`);
const pv = new Point();
console.log(pv);
const o2 = { q: 1 };
const list = [o2];
const la = list;
console.log(`through an any element: ${la[0] === o2}`);
const xs = [1, 2];
console.log(`isArray: ${Array.isArray(xs)}`);
