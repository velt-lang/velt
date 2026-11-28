// Node twin of any_call_caught_typed_sort.velt: the same program with its types removed.
function check(x) {
  if (x < 0) throw new RangeError("negative " + x);
  return x;
}
const table = { check };
console.log(table.check(1));

const nums = [3, 1, 2];
console.log(nums.toSorted().join(","), nums.toSorted((a, b) => b - a).join(","));
const strs = ["b", "c", "a"];
console.log(strs.toSorted().join(","), strs.toSorted((a, b) => (a < b ? 1 : -1)).join(","));
const floats = new Float64Array([2, 1]);
console.log(floats.toSorted().join(","));

function weight(x) {
  return table.check(x);
}
nums.sort((a, b) => weight(a) - weight(b));
console.log(nums.join(","), nums.toSorted((a, b) => weight(b) - weight(a)).join(","));
const signed = [2, -1, 3];
try {
  signed.sort((a, b) => weight(a) - weight(b));
} catch (e) {
  console.log("helper caught", e.message, signed.join(","));
}
try {
  console.log(signed.toSorted((a, b) => weight(a) - weight(b)).join(","));
} catch (e) {
  console.log("toSorted caught", e.message, signed.join(","));
}

class Order {
  constructor(k) {
    this.k = k;
  }
  cmp(a, b) {
    return this.k.check(a) - this.k.check(b);
  }
}
const ord = new Order(table);
const more = [9, 8];
more.sort((a, b) => ord.cmp(a, b));
console.log(more.join(","));
try {
  signed.sort((a, b) => ord.cmp(b, a));
} catch (e) {
  console.log("method caught", e.message);
}

const three = [3, 2, 1];
three.sort((a, b) => {
  const g = () => table.check(a);
  return g() - b;
});
console.log(three.join(","));

const byWeight = (a, b) => weight(b) - weight(a);
three.sort(byWeight);
console.log(three.join(","));
function byCheck(a, b) {
  return weight(a) - weight(b);
}
three.sort(byCheck);
console.log(three.join(","), three.toSorted(byWeight).join(","));
try {
  signed.sort(byCheck);
} catch (e) {
  console.log("named caught", e.message, signed.join(","));
}

function intWeight(x) {
  return table.check(x);
}
function byInt(a, b) {
  return intWeight(a) - intWeight(b);
}
const ints = [3, 1, 2];
ints.sort(byInt);
console.log(ints.join(","), ints.toSorted((a, b) => intWeight(b) - intWeight(a)).join(","));
const badInts = [1, -4];
try {
  badInts.sort(byInt);
} catch (e) {
  console.log("i64 caught", e.message, badInts.join(","));
}

function plain(a, b) {
  return a - b;
}
signed.sort(plain);
console.log(signed.join(","));
