// Node twin of any_call_caught_sort_method.velt: the same program with its types removed.
function check(x) {
  if (x < 0) throw new RangeError("negative " + x);
  return x;
}
const table = { check };
console.log(table.check(1));
function weight(x) {
  return table.check(x);
}
class Order {
  by(a, b) {
    return weight(b) - weight(a);
  }
}
const ob = new Order();
const xs = [3, 1, 2];
console.log(xs.toSorted(ob.by).join(","));
xs.sort(ob.by);
console.log(xs.join(","));
const bad = [2, -1];
try {
  bad.sort(ob.by);
} catch (e) {
  console.log("method caught", e.message, bad.join(","));
}
