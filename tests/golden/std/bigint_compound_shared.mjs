// Node twin of bigint_compound_shared.velt: prints its .out. (`addAssign` changes the number in
// place; JS has no such method, so the last line is written out.)
const n = (k) => BigInt(k);

const x = n(5);
let z = x;
z += n(1);
console.log(x.toString(), z.toString());

const a = n(100);
let b = a;
b -= n(1);
let c = a;
c *= n(3);
let d = a;
d /= n(7);
let e = a;
e %= n(7);
let f = a;
f <<= 2n;
let g = a;
g >>= 2n;
console.log(a.toString(), b.toString(), c.toString(), d.toString());
console.log(e.toString(), f.toString(), g.toString());

class Acc {
  total = n(0);
}
const acc = new Acc();
const start = acc.total;
const xs = [n(1), n(2)];
const first = xs[0];
for (let i = 1; i <= 3; i++) {
  acc.total += n(i);
  xs[0] *= n(10);
}
console.log(start.toString(), acc.total.toString(), first.toString(), xs[0].toString());

let y = n(7);
y += y;
y *= y;
console.log(y.toString());

console.log("42", "42");

// Shapes where a number is held elsewhere: each holder keeps its value.
const s = (b) => b.toString();
const held = n(1);
const keep = () => s(held);
let c1 = held;
c1 += n(1);
let c2 = n(5);
const later = () => s(c2);
const c2old = c2;
c2 += n(1);
console.log(s(held), s(c1), keep(), later(), s(c2old));

const arr = [n(10)];
let e1 = arr[0];
e1 += n(1);
const map = new Map();
map.set("k", n(20));
let e2 = map.get("k");
e2 *= n(2);
const box = new Acc();
box.total = n(30);
let e3 = box.total;
e3 -= n(3);
console.log(s(arr[0]), s(e1), s(map.get("k")), s(e2), s(box.total), s(e3));

function bump(p) {
  let q = p;
  q += n(1);
  return s(q);
}
const arg = n(40);
console.log(bump(arg), s(arg));

let self = n(3);
const selfOld = self;
self += self;
self *= self;
console.log(s(self), s(selfOld));

for (const el of arr) {
  let w = el;
  w += n(5);
  console.log(s(w), s(arr[0]));
}
const [d0] = [n(50)];
let d1 = d0;
d1 %= n(7);
console.log(s(d0), s(d1));

function twice(x) {
  return [x, x];
}
const pair = twice(n(60));
let g0 = pair[0];
g0 += n(1);
console.log(s(pair[0]), s(pair[1]), s(g0));

async function stepAsync() {
  let a = n(70);
  const before = a;
  await null;
  a += n(1);
  await null;
  let b = n(80);
  await null;
  b += n(1);
  return `${s(before)} ${s(a)} ${s(b)}`;
}

let unique = n(1);
for (let i = 0; i < 5; i++) {
  unique *= n(3);
  unique += n(i);
}
console.log(s(unique));


let r = n(1);
const reassign = () => {
  r = n(100);
  return n(2);
};
r += reassign();
console.log(s(r));

let t = n(1);
t += (t = n(5));
console.log(s(t));

// The operator methods of the .velt file, called as functions.
let sneaked = n(0);
let sz = n(1);
const sa = () => {
  sneaked = sz;
  return 2;
};
sz += n(sa() + sa() - 2);
console.log(s(sneaked), s(sz));
let sy = n(10);
const sb = () => {
  sneaked = sy;
  return 3;
};
sy *= n(sb());
console.log(s(sneaked), s(sy));
const sneakStore = [];
function sneakIn() {
  let z = n(1);
  const read = () => z;
  const a = () => {
    sneakStore.push(read());
    return 2;
  };
  z += n(a() + a() - 2);
  let y = n(10);
  const readY = () => y;
  const b = () => {
    sneakStore.push(readY());
    return 3;
  };
  y *= n(b());
  console.log(s(sneakStore[0]), s(z), s(sneakStore[2]), s(y));
}
sneakIn();
async function main() {
  console.log(await stepAsync());
}
main();
