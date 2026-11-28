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
console.log(a.toString(), b.toString(), c.toString(), d.toString(), e.toString(), f.toString(), g.toString());

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
