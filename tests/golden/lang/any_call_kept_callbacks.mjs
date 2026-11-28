// Node twin of any_call_kept_callbacks.velt: the same program with its types removed.
function check(x) {
  if (x < 0) throw new RangeError("negative");
  return x;
}
const table = { check };

const nums = [1, 2, 3];
console.log(nums.find((n) => table.check(n) > 1));
console.log(nums.some((n) => table.check(n) > 2));
console.log(nums.every((n) => table.check(n) > 0));
console.log(nums.filter((n) => table.check(n) > 1).join(","));
console.log(nums.reduce((s, n) => s + table.check(n), 0));
console.log(nums.findIndex((n) => table.check(n) == 2));
console.log(nums.flatMap((n) => [table.check(n)]).join(","));
console.log(Array.from(nums, (n) => table.check(n) * 2).join(","));
const m = new Map([["a", 1]]);
m.forEach((v, k) => console.log(k, table.check(v)));

class Hooks {
  onMount = (r) => {};
}
const hooks = new Hooks();
hooks.onMount = (r) => console.log("mount", r.name, table.check(1));
hooks.onMount({ name: "m" });
let cb = (n) => table.check(n);
console.log(cb(5));
cb = (n) => {
  try {
    return table.check(n);
  } catch (e) {
    console.log("inside", e.message);
    return 0;
  }
};
console.log(cb(-5));

try {
  table.check(-1);
} catch (e) {
  console.log("caught", e.message);
}
