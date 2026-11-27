// Node twin of number_literal_mixed_union_as_number.velt: the same program without its types.
function step(i) {
  return i > 0 ? 2.5 : 1;
}

let n = 1;
const k = 2.5;
n += k;
n *= k;
console.log("+= *=", n, k.toFixed(2));
const u = step(0);
n -= u;
n /= step(1);
n **= u;
n %= k;
console.log("-= /= **= %=", n);
console.log("methods", u.toFixed(1), k.toPrecision(3), k.toExponential(1), u.toExponential(2));

const o = { k: 1.5 };
let t = 1;
if (o.k !== undefined) {
  t += o.k;
}
const none = {};
if (none.k !== undefined) {
  t += none.k;
}
console.log("optional", t);

const ks = [0, 1, 2.5];
const xs = [...ks];
const ys = [10, ...ks];
ys.push(...ks);
console.log("spread", xs.join(","), ys.join(","), Math.max(...ks));
console.log(
  "callbacks",
  ks.reduce((a, x) => a + x, 0),
  ks.map((x) => x * 2).join(","),
  ks.map((x) => x.toFixed(1)).join(" "),
);
