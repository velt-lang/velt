// Node twin of number_literal_union_as_number.velt: the same program without its types.
function pick(i) {
  return i > 0 ? 2.5 : 1.5;
}

function small(i) {
  return i > 1 ? 2 : i > 0 ? 1 : 0;
}

function twice(n) {
  return n * 2;
}

function add(...xs) {
  return xs.reduce((a, b) => a + b, 0);
}

// Compound assignments to a `number` with a literal-typed operand.
const k = 2.5;
const one = 2.5;
const w = pick(0);
const j = small(2);
let c = 1;
c += k;
c += one;
c += w;
console.log("+=", c);
c -= k;
c -= w;
console.log("-=", c);
c *= k;
c *= w;
console.log("*=", c);
c /= w;
c /= k;
console.log("/=", c);
c **= k;
console.log("**=", c);
c %= w;
console.log("%=", c);
c += j;
c *= j;
c **= j;
c -= j;
c /= j;
console.log("int", c);
let f = 0.5;
for (let i = 0; i < 3; i++) {
  f += pick(i);
  f *= small(i) + 1;
}
console.log("loop", f);

// Spreads into a `number[]` and `(number | null)[]`.
const ks = [0, 1, 2];
const hs = [1.5, 2.5];
const ns = [...ks];
const ms = [0.5, ...ks, ...hs, 3];
const ps = [];
ps.push(...ks);
ps.push(...hs, 7);
const qs = [null, ...ks];
console.log(ns, ms, ps, qs);
console.log(Math.max(...ns), ns.map((n) => n / 4), ms.reduce((a, b) => a + b, 0));
// From an iterable, and as the arguments of a `...number[]` parameter.
function* bits3() {
  yield 1;
  yield 2;
}
const seen = new Set([2, 0]);
const gs = [...bits3()];
const ss3 = [0.5, ...seen];
console.log(gs, ss3, Math.max(...ks), Math.min(...hs), add(...ks), add(1, ...hs));

// Number methods on a literal-typed receiver.
const m = small(2);
const z = 0;
console.log(m.toFixed(1), k.toFixed(2), w.toFixed(0), z.toFixed(3), one.toFixed(1));
console.log(m.toPrecision(3), k.toPrecision(2), m.toExponential(1), k.toExponential());
console.log(m.toString(), m.toString(2), k.toString(), small(1).toFixed(2), pick(1).toFixed(1));

// A literal union as a `number` argument, in a ternary, and in a template.
const t = c > 0 ? k : j;
console.log(twice(k), twice(j), t, `${k}|${j}|${one}`, String(m) + "!");

// Other values a `number` takes: a `readonly number[]` argument, a fresh array, a generic's
// array, a field, an element, a record's value and a bitwise operand.
function sum(xs) {
  let total = 0;
  for (const x of xs) total += x;
  return total;
}
function make() {
  return [2, 1];
}
function wrap(x) {
  return [x];
}
class Acc {
  total = 1;
  add() {
    this.total += k;
    this.total *= j;
  }
}
const fresh = make();
const wrapped = wrap(j);
const acc = new Acc();
acc.add();
const xs = [1, 2];
xs[0] += k;
xs[1] *= j;
const r = { a: 1 };
r["a"] += w;
let bits = 4;
bits |= j;
bits &= m + 1;
console.log(sum(ks), sum(hs), sum(make()), fresh, wrapped, acc.total, xs, r, bits);

// A union of number and string literals where a `string` or `number | string` takes it.
const mx = small(1) > 0 ? "a" : 1;
const mxs = [1, "a"];
const sn = [...mxs, 2];
let s0 = "s";
s0 += mx;
console.log(s0, sn, `${mx}`);

// A string literal union where a `string` takes it.
const tag = "b";
let s = "x";
s += tag;
const tags = ["a", "b"];
const ss = [...tags, "c"];
ss.push(...tags);
console.log(s, ss, tag.toUpperCase(), tag.padStart(3, "-"));
