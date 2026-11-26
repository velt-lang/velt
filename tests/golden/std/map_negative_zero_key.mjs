function sign(x) {
  return 1 / x > 0 ? "+0" : "-0";
}

const m = new Map([[-0, "neg"]]);
const k = [...m.keys()][0];
console.log(sign(k), m.get(0), m.get(-0), m.size);
m.set(0, "pos");
console.log(sign([...m.keys()][0]), m.get(-0), m.size);

const s = new Set([-0]);
console.log(sign([...s][0]), s.has(0), s.has(-0));
s.add(0);
console.log(s.size);

const u = new Set([1]).union(new Set([-0]));
console.log([...u].map((x) => (x == 0 ? sign(x) : `${x}`)).join(","));

const f = new Map();
f.set(-0, 1);
console.log(sign([...f.keys()][0]));

const n = new Map([[NaN, 1]]);
console.log(Number.isNaN([...n.keys()][0]), n.get(NaN));

const t = new Map([["-0", 1]]);
console.log([...t.keys()][0]);

function show(x) {
  return typeof x === "number" && x == 0 ? sign(x) : `${x}`;
}

const uk = -0;
const um = new Map([[uk, 1], ["a", 2]]);
console.log([...um.keys()].map(show).join(","), sign(uk), um.get(0), um.get(-0));

const o = new Set([-0, undefined]);
console.log([...o].map(show).join(","), o.has(0));

const b = new Set([true, -0]);
console.log([...b].map((x) => (typeof x === "number" ? sign(x) : `${x}`)).join(","));

const ak = -0;
const am = new Map([[ak, 1], ["s", 2], [0, 3], [true, 4]]);
console.log(sign([...am.keys()][0]), sign(ak), am.get(0), am.size);
console.log([...am.keys()].map((x) => `${x}`).join(","), [...am.values()].join(","));

const nk = -0;
const ns = new Set([nk, NaN, 5, { k: -0 }]);
console.log(sign([...ns][0]), sign(nk), ns.has(0), ns.size);
console.log(sign([...ns][3].k));

function single(x) {
  return [...new Set([x])];
}
console.log(sign(single(-0)[0]));
