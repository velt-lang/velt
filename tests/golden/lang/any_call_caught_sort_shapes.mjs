// Node twin of any_call_caught_sort_shapes.velt: the same program with its types removed.
function check(x) {
  if (x < 0) throw new RangeError("negative " + x);
  return x;
}
const table = { check };
console.log(table.check(1));
function weight(x) {
  return table.check(x);
}
function plain(a, b) {
  return b - a;
}
function heavy(a, b) {
  return weight(a) - weight(b);
}

const cmpObj = { by: (a, b) => weight(a) - weight(b) };
const xs = [3, 1, 2];
xs.sort(cmpObj.by);
console.log(xs.join(","));

function mk() {
  return (a, b) => weight(a) - weight(b);
}
const ys = [2, 3, 1];
ys.sort(mk());
console.log(ys.join(","));

for (const desc of [true, false]) {
  const zs = [1, 3, 2];
  zs.sort(desc ? plain : heavy);
  console.log(zs.join(","));
}

const fa = new Float64Array([3, 1, 2]);
fa.sort((a, b) => weight(a) - weight(b));
console.log(fa.join(","), fa.toSorted((a, b) => weight(b) - weight(a)).join(","));
const ia = new Int32Array([2, 3, 1]);
ia.sort((a, b) => weight(a) - weight(b));
console.log(ia.join(","));

const bad = [2, -1, 3];
try {
  bad.sort(cmpObj.by);
} catch (e) {
  console.log("member caught", e.message, bad.join(","));
}
try {
  bad.sort(mk());
} catch (e) {
  console.log("call caught", e.message, bad.join(","));
}
const badTyped = new Float64Array([2, -5, 3]);
try {
  badTyped.sort((a, b) => weight(a) - weight(b));
} catch (e) {
  console.log("typed array caught", e.message, badTyped.join(","));
}
try {
  console.log(new Int32Array([1, -6]).toSorted((a, b) => weight(a) - weight(b)).join(","));
} catch (e) {
  console.log("toSorted caught", e.message);
}

const rows = [{ k: 2 }, { k: 1 }];
rows.sort((a, b) => a.k - b.k);
console.log(rows.map((r) => r.k).join(","));
