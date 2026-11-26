let log = [];

class L extends Set {
  tag = "";
  constructor(xs, tag) {
    super(xs);
    this.tag = tag;
  }
  get size() {
    log.push(`${this.tag}.size`);
    return super.size;
  }
  has(x) {
    log.push(`${this.tag}.has(${x})`);
    return super.has(x);
  }
  keys() {
    log.push(`${this.tag}.keys`);
    return super.keys();
  }
  values() {
    log.push(`${this.tag}.values`);
    return super.values();
  }
}

function elems(r) {
  const out = [];
  r.forEach((x) => {
    out.push(x);
  });
  return out.join(",");
}

function showSet(name, r) {
  const calls = log.join(" ");
  log = [];
  console.log(`${name}: [${elems(r)}] ${r instanceof L} | ${calls}`);
}

function showBool(name, r) {
  const calls = log.join(" ");
  log = [];
  console.log(`${name}: ${r} | ${calls}`);
}

function mk() {
  return new L([1, 2, 3], "a");
}

function run(label, other) {
  console.log(`-- ${label}`);
  let a = mk();
  let o = other();
  log = [];
  showSet("union", a.union(o));
  a = mk();
  o = other();
  log = [];
  showSet("intersection", a.intersection(o));
  a = mk();
  o = other();
  log = [];
  showSet("difference", a.difference(o));
  a = mk();
  o = other();
  log = [];
  showSet("symmetricDifference", a.symmetricDifference(o));
  a = mk();
  o = other();
  log = [];
  showBool("isSubsetOf", a.isSubsetOf(o));
  a = mk();
  o = other();
  log = [];
  showBool("isSupersetOf", a.isSupersetOf(o));
  a = mk();
  o = other();
  log = [];
  showBool("isDisjointFrom", a.isDisjointFrom(o));
}

run("bigger", () => new L([2, 3, 4, 5], "b"));
run("smaller", () => new L([2], "c"));
run("early", () => new L([1, 5, 6, 7], "d"));

const a = mk();
log = [];
const seen = [];
for (const x of a) seen.push(x);
showBool(`for-of ${seen.join(",")}`, false);
const spread = [...a];
showBool(`spread ${spread.join(",")}`, false);
const ks = new Set([3, 1, 3]);
console.log([...ks.keys()].join(","), [...ks.entries()].map((p) => `${p[0]}:${p[1]}`).join(" "));
