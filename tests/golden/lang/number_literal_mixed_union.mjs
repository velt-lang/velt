// Node twin of number_literal_mixed_union.velt: the same program with its types removed.
// A union of integer and float literals (`0 | 1 | 2.5`) is a `number` everywhere one is
// expected and to every operator, as in TypeScript (#3815): its members have two base types
// underneath (`i64` and `f64`), where `0 | 1` or `0.5 | 1.5` has one. `===` and `switch` still
// compare it as the union it is.

function g(u) {
  const x = u;
  console.log(x, u * 2, -u, +u, ~u, u / 4, u + 1, u - 0.5, u % 2, u ** 2);
  console.log(u | 0, u & 3, u ^ 1, u << 1, u >> 1, u >>> 0);
  console.log(u === 1, u === 2.5, u == 0, u < 2, u >= 1, typeof u, `${u}`, String(u));
}

g(0);
g(1);
g(2.5);

function half(n) {
  return n / 2;
}

function back(u) {
  return u;
}

function maybe(u) {
  return u;
}

function either(u) {
  return u;
}

const all = [0, 1, 2.5];
let sum = 0;
let bits = 0;
for (const u of all) {
  sum += u;
  bits |= u;
  console.log(half(u), back(u), maybe(u), either(u), Math.max(u, 1), Math.floor(u));
}
console.log(sum, bits, all.map((u) => u * 10), all.reduce((a, b) => a + b, 0));

class Holder {
  v = 2.5;
  total() {
    return this.v * 2 + -this.v;
  }
}

console.log(new Holder().total(), -new Holder().v);
const neg = -1;
console.log(-neg, neg * 3, neg + 0.25);

function label(u) {
  switch (u) {
    case 0:
      return "zero";
    case 1:
      return "one";
    case 2.5:
      return "two and a half";
  }
}

console.log(all.map(label).join(", "));
let acc = 1;
for (const u of all) {
  acc *= u + 1;
  acc -= u;
  acc ^= u;
}
console.log(acc);
