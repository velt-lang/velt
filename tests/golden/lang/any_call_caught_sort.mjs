// Node twin of any_call_caught_sort.velt: the same program with its types removed.
function check(x) {
  if (x < 0) throw new RangeError("negative");
  return x;
}
const table = { check };

const rows = [{ name: "b" }, { name: "a" }, { name: "c" }];
rows.sort((a, b) => a.name.localeCompare(b.name));
console.log(rows.map((r) => r.name).join(","));
const back = rows.toSorted((a, b) => b.name.localeCompare(a.name));
console.log(back.map((r) => r.name).join(","));

function byName(a, b) {
  return a.name.localeCompare(b.name);
}
rows.reverse();
rows.sort(byName);
console.log(rows.map((r) => r.name).join(","));

const typed = [{ name: "y" }, { name: "x" }, { name: "z" }];
const order = { rank: (r) => r.name.charCodeAt(0) };
typed.sort((a, b) => order.rank(a) - order.rank(b));
console.log(typed.map((r) => r.name).join(","));
const nums = [3, 1, 2];
nums.sort((a, b) => table.check(a) - table.check(b));
console.log(nums.join(","));

function byRank(a, b) {
  return order.rank(b) - order.rank(a);
}
function byCheck(a, b) {
  return table.check(b) - table.check(a);
}
typed.sort(byRank);
console.log(typed.map((r) => r.name).join(","));
nums.sort(byCheck);
console.log(nums.join(","), nums.toSorted(byCheck).join(","));
nums.sort(function (a, b) {
  return table.check(a) - b;
});
console.log(nums.join(","));
const ints = [3, 1, 2];
ints.sort((a, b) => a - b);
console.log(ints.join(","));
console.log(typed.toSorted(byRank).map((r) => r.name).join(","));

nums.reverse();
nums.sort((a, b) => {
  try {
    return table.check(-a) - b;
  } catch (e) {
    return a - b;
  }
});
console.log(nums.join(","));

try {
  table.check(-1);
} catch (e) {
  console.log("caught", e.message);
}
try {
  rows.sort((a, b) => table.check(-1));
} catch (e) {
  console.log("sort caught", e.message);
}
console.log(rows.map((r) => r.name).join(","));
try {
  rows.toSorted((a, b) => table.check(-2));
} catch (e) {
  console.log("toSorted caught", e.message);
}
