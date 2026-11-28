// Node twin of this_fn_call_object_order.velt: the same program with its types removed.
const m = function (x) {
  return `${this.name}:${x}`;
};

let o = { name: "old", m: m, n: 5 };
const a = { b: { name: "old b", m: m } };
let arr = [m];

function log(what, value) {
  console.log(what);
  return value;
}

function swap() {
  o = { name: "new", m: m };
  return 1;
}

function swapB() {
  a.b = { name: "new b", m: m };
  return 2;
}

function swapArr() {
  arr = [(x) => `new arr:${x}`];
  return 3;
}

function key() {
  console.log("key");
  return "m";
}

async function later() {
  await Promise.resolve(0);
  o = { name: "after await", m: m };
  return 4;
}

async function main() {
  console.log(o["m"](swap()));
  o = { name: "old", m: m, n: 5 };
  console.log(a.b["m"](swapB()));
  console.log(o[key()](log("arg", 6)));
  o = { name: "old", m: m, n: 5 };
  console.log(o.m?.(swap()));
  o = { name: "old", m: m, n: 5 };
  console.log(o["m"](await later()));
  console.log(o.name);
  console.log(arr[0](swapArr()));
  o = { name: "old", m: m, n: 5 };
  try {
    o["n"](swap());
  } catch (e) {
    console.log(e.message, o.name);
  }
}

main();
