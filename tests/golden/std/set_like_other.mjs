const a = new Set([1, 2, 3]);
const m = new Map([
  [2, "two"],
  [7, "seven"],
]);
console.log([...a.union(m)].join(","), [...a.intersection(m)].join(","), a.isSubsetOf(m), a.isDisjointFrom(m));

let log = [];

/** A set-like value that logs what the algebra reads of it. */
class Like {
  xs;
  constructor(xs) {
    this.xs = xs;
  }
  get size() {
    log.push("size");
    return this.xs.length;
  }
  has(value) {
    log.push(`has(${value})`);
    return this.xs.includes(value);
  }
  keys() {
    log.push("keys");
    return this.xs.values();
  }
}

function show(name, r) {
  console.log(`${name}: ${r} | ${log.join(" ")}`);
  log = [];
}

function elems(s) {
  return [...s].join(",");
}

for (const xs of [[3, 8], [1, 2, 3, 4, 5], [1, 9]]) {
  console.log(`-- other ${xs.join(",")}`);
  log = [];
  show("union", elems(a.union(new Like(xs))));
  show("intersection", elems(a.intersection(new Like(xs))));
  show("difference", elems(a.difference(new Like(xs))));
  show("symmetricDifference", elems(a.symmetricDifference(new Like(xs))));
  show("isSubsetOf", `${a.isSubsetOf(new Like(xs))}`);
  show("isSupersetOf", `${a.isSupersetOf(new Like(xs))}`);
  show("isDisjointFrom", `${a.isDisjointFrom(new Like(xs))}`);
}

const lit = {
  size: 2,
  has(x) {
    return x == 3 || x == 8;
  },
  keys() {
    return [3, 8].values();
  },
};
console.log([...a.union(lit)].join(","), [...a.difference(lit)].join(","), a.isSupersetOf(lit));
