// Node twin of array_view_negzero.velt: the casts are no-ops in TypeScript.
const e = [0, 5];
e[0] = -0;
console.log(`-0 over 0: ${Object.is(e[0], -0)}`);

const g = [-0, 1];
g[0] = 0;
console.log(`0 over -0: ${Object.is(g[0], -0)}`);

const h = [0, 2];
const hv = h;
console.log(`before: ${Object.is(hv[0], -0)}`);
h[0] = -0;
console.log(`behind the view: ${Object.is(hv[0], -0)}`);

const z = [-0, 3, 4];
const zv = z;
zv.fill(9, 1);
zv.sort((a, b) => 0);
console.log(`u8 view, -0 kept: ${Object.is(z[0], -0)} ${z[1]} ${z[2]}`);

const q = [0, 1];
q.fill(-0, 0, 1);
console.log(`fill -0 over 0: ${Object.is(q[0], -0)}`);

const a = [-0, 1];
a.fill(0);
console.log(`fill 0 over -0: ${Object.is(a[0], -0)}`);

const s = [0, -0, 1];
const negFirst = (x, y) => (Object.is(x, -0) ? -1 : Object.is(y, -0) ? 1 : 0);
s.sort(negFirst);
console.log(`sort, zeros swap: ${Object.is(s[0], -0)} ${Object.is(s[1], -0)} ${s[2]}`);

const r = [-0, 0];
r.reverse();
console.log(`reverse: ${Object.is(r[0], -0)} ${Object.is(r[1], -0)}`);

const show = (xs) => xs.map((x) => (Object.is(x, -0) ? "-0" : `${x}`)).join(",");
const g2 = [-0, 1];
const gv = g2;
gv[0] = gv[0];
console.log(`i32 view, -0 written back: ${Object.is(g2[0], -0)}`);
const h2 = [-0, 7, 2, -0];
h2.reverse();
console.log(`i32 view reverse: ${show(h2)}`);

const childNegFirst = (p, q) => (Object.is(p, -0) ? -1 : 0) - (Object.is(q, -0) ? -1 : 0);
const w1 = [0, -0, 0, -0];
w1.sort(childNegFirst);
console.log(`Child sort: ${show(w1)}`);
const w2 = [0, -0, "a"];
w2.shift();
console.log(`Child shift: ${show(w2)}`);
const w3 = [0, 5];
const w3v = w3;
console.log(`Child read: ${w3v[0]}`);
w3[0] = -0;
w3v.sort(childNegFirst);
console.log(`Child sort after a change behind: ${show(w3)}`);
