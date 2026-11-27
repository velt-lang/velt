class Fixed extends Set {
  [Symbol.iterator]() {
    return [100, 200][Symbol.iterator]();
  }
}
class Plain extends Set {}

function each(s) {
  const out = [];
  for (const x of s) {
    out.push(x);
  }
  return out.join(",");
}

const asSet = new Fixed();
asSet.add(1);
const viaVar = [];
for (const x of asSet) {
  viaVar.push(x);
}
const fixed = new Fixed();
fixed.add(2);
const direct = [];
for (const x of fixed) {
  direct.push(x);
}
console.log(viaVar.join(","), direct.join(","), each(new Fixed()));

const p = new Plain();
p.add(5);
p.add(6);
const ps = [];
for (const x of p) {
  ps.push(x);
  if (x == 5) {
    p.delete(6);
    p.add(7);
  }
}
const s = new Set([1, 2, 3]);
const ss = [];
for (const x of s) {
  ss.push(x);
  if (x == 1) {
    s.delete(2);
    s.add(9);
  }
}
console.log(ps.join(","), ss.join(","), each(p), each(s));
