// Node twin of record_view_static.velt: the casts are no-ops in TypeScript.
class Point {
  x = 1;
  y = 2;
}

const o = { a: 1, b: "x" };
const ov = o;
console.log(`literal: ${ov === o} ${ov !== o} ${Object.is(ov, o)} ${Object.is(o, ov)}`);
const p = new Point();
const pv = p;
console.log(`class: ${pv === p} ${p === pv} ${pv !== p} ${Object.is(pv, p)}`);
const other = new Point();
console.log(`other class: ${pv === other} ${Object.is(pv, other)}`);
const xs = [1, 2];
const xv = xs;
console.log(`array: ${xv === xs} ${Object.is(xv, xs)} ${JSON.stringify(xv)}`);
class D {
  toJSON(k) {
    return "key=" + k;
  }
}

const n = { name: "n" };
const nv = n;
console.log(`interface: ${nv === n} ${Object.is(nv, n)}`);
const m = new Map();
const mv = m;
console.log(`map: ${mv === m} ${Object.is(mv, m)}`);
console.log(`stringify object: ${JSON.stringify(ov)} ${JSON.stringify(pv)}`);
const plain = { a: 1 };
console.log(`plain record: ${plain === p} ${plain !== p} ${JSON.stringify(plain)}`);
console.log(`nested view: ${JSON.stringify({ a: xv })} ${JSON.stringify(mv)}`);
const dv = new D();
console.log(`toJSON key: ${JSON.stringify({ k: dv })} ${JSON.stringify([dv])}`);
const full = new Map([["a", 1]]);
const fullAny = full;
const fullView = full;
console.log(
  `stringify map: ${JSON.stringify(fullView)} ${JSON.stringify(fullAny)} ${JSON.stringify({ m: fullView })}`,
);
const set = new Set([1, 2]);
const setAny = set;
const setView = set;
console.log(
  `stringify set: ${JSON.stringify(setView)} ${JSON.stringify(setAny)} ${JSON.stringify([setView])}`,
);
