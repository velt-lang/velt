// The Node twin of tests/golden/std/json_stringify_map.velt: Node writes a `Map`, a `Set`, a
// `WeakMap` and a `WeakSet` as `{}` (no enumerable own properties). The struct is a class
// here, and `JSON.parse<Map<string, T>>` is `new Map(Object.entries(JSON.parse(text)))`.

class Point {
  constructor(x, tags) {
    this.x = x;
    this.tags = tags;
  }
}

class Holder {
  name = "h";
  counts = new Map();
}

class Tagged extends Map {
  label = "t";
}

class Listed extends Map {
  toJSON() {
    return [...this.entries()];
  }
}

class Bag extends Set {
  owner = "me";
}

class Wrap {
  toJSON() {
    const m = new Map();
    m.set("a", 1);
    return m;
  }
}

class Key {
  id = 1;
}

function sample() {
  const m = new Map();
  m.set("a", 1);
  m.set("b", 2);
  return m;
}

function typed() {
  const m = sample();
  console.log(JSON.stringify(m));
  const p = new Point(1, sample());
  console.log(JSON.stringify(p));
  const h = new Holder();
  h.counts.set("k", 3);
  console.log(JSON.stringify(h));
  console.log(JSON.stringify([m, m]));
  const inner = new Map([["x", "y"]]);
  const d = { title: "d", counts: m, nested: { inner: inner } };
  console.log(JSON.stringify(d));
  console.log(JSON.stringify({ m: m, n: 2 }));
  const byNumber = new Map();
  byNumber.set(1, "one");
  console.log(JSON.stringify(byNumber));
  const opt = m;
  console.log(JSON.stringify({ opt: opt }));
}

function dynamic() {
  const a = sample();
  console.log(JSON.stringify(a));
  const o = { m: sample(), n: 2 };
  console.log(JSON.stringify(o));
  const arr = [sample(), new Set([1, 2])];
  console.log(JSON.stringify(arr));
}

function others() {
  const s = new Set([1, 2]);
  console.log(JSON.stringify(s));
  console.log(JSON.stringify({ s: s }));
  const sa = s;
  console.log(JSON.stringify(sa));
  const wm = new WeakMap();
  const k = new Key();
  wm.set(k, 1);
  console.log(JSON.stringify(wm));
  const wma = wm;
  console.log(JSON.stringify(wma));
  const ws = new WeakSet();
  ws.add(k);
  console.log(JSON.stringify(ws));
  const t = new Tagged();
  t.set("a", 1);
  console.log(JSON.stringify(t));
  const l = new Listed();
  l.set("a", 1);
  console.log(JSON.stringify(l));
  const b = new Bag();
  b.add("x");
  console.log(JSON.stringify(b));
  console.log(JSON.stringify(new Wrap()));
  console.log(JSON.stringify({ w: new Wrap() }));
  // A subclass's instance seen as the base collection is written as its class.
  const tm = t;
  console.log(JSON.stringify(tm), JSON.stringify({ m: tm }));
  const lm = l;
  console.log(JSON.stringify(lm), JSON.stringify([lm]));
  const bs = b;
  console.log(JSON.stringify(bs), JSON.stringify({ s: bs }));
}

function options() {
  const m = sample();
  const d = { title: "d", counts: m, nested: { inner: new Map() } };
  console.log(JSON.stringify(m, null, 2));
  console.log(JSON.stringify(d, null, 2));
  console.log(JSON.stringify(d, ["title", "counts", "a"]));
  console.log(JSON.stringify(d, (k, v) => v));
  console.log(JSON.stringify(d, (k, v) => (k == "counts" ? undefined : v)));
  console.log(JSON.stringify(m, (k, v) => v, 1));
}

function entries() {
  const m = sample();
  console.log(JSON.stringify(Object.fromEntries(m)));
  console.log(JSON.stringify({ counts: Object.fromEntries(m) }));
  const back = new Map(Object.entries(JSON.parse('{"a":1,"b":2}')));
  console.log(back.size, back.get("b"));
  console.log(JSON.stringify(Object.fromEntries(back)));
}

typed();
dynamic();
others();
options();
entries();
