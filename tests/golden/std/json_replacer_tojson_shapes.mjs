// The Node twin of tests/golden/std/json_replacer_tojson_shapes.velt. `view` prints what the
// replacer got as Velt's `JsonValue.stringify()` prints its view: the value as JSON with no
// `toJSON` called (a `Map` has no enumerable own properties: `{}`). `missing` is not declared:
// Node passes the replacer an own property holding `undefined`, which Velt's replacer, given a
// `JsonValue`, cannot get (#924), so the golden's absent optional field is not visited.

function view(v) {
  if (v instanceof Date) {
    return JSON.stringify(v);
  }
  if (Array.isArray(v)) {
    return `[${v.map((e) => (e === undefined || typeof e === "function" ? "null" : view(e))).join(",")}]`;
  }
  if (v !== null && typeof v === "object") {
    const parts = [];
    for (const k of Object.keys(v)) {
      if (v[k] === undefined || typeof v[k] === "function") {
        continue;
      }
      parts.push(`${JSON.stringify(k)}:${view(v[k])}`);
    }
    return `{${parts.join(",")}}`;
  }
  return JSON.stringify(v);
}

class Hook {
  constructor(name) {
    this.name = name;
  }
  toJSON(key) {
    console.log(`  toJSON ${this.name} key=${JSON.stringify(key)}`);
    return this.name;
  }
}

class Base {
  kind = "base";
  toJSON(key) {
    console.log(`  toJSON base key=${JSON.stringify(key)}`);
    return "base";
  }
}

class Derived extends Base {
  extra = new Hook("x");
  toJSON(key) {
    console.log(`  toJSON derived key=${JSON.stringify(key)}`);
    return "derived";
  }
}

class Shapes {
  either = new Hook("u");
  plain = "s";
  rec = { r1: new Hook("r1"), r2: new Hook("r2") };
  pair = [new Hook("t0"), 7];
  present = new Hook("o");
  map = new Map([["m", new Hook("m")]]);
  base = new Derived();
  dyn = new Hook("i");
}

function logged(k, v) {
  console.log(`  replacer ${JSON.stringify(k)} ${view(v)}`);
  return v;
}

function edited(k, v) {
  console.log(`  replacer ${JSON.stringify(k)} ${view(v)}`);
  if (k == "y" || k == "1") {
    return undefined;
  }
  if (k == "s") {
    return "S";
  }
  return v;
}

function generic(x) {
  return JSON.stringify(x, logged);
}

console.log("shapes:");
console.log(JSON.stringify(new Shapes(), logged));
console.log("generic:");
console.log(generic([new Hook("g")]));
console.log("space:");
console.log(JSON.stringify({ k: new Hook("sp") }, logged, "-"));
console.log("plain:");
const mixed = {
  n: 1.5,
  s: "t",
  b: true,
  list: [1, 2, 3],
  pt: { x: 1, y: 2 },
  pair: [3, "p"],
  h: new Hook("h"),
};
console.log(JSON.stringify(mixed, edited));
const points = [{ x: 1, y: 2 }, { x: 3, y: 4 }];
console.log(JSON.stringify(points, edited));
console.log(JSON.stringify("top", logged));
console.log(JSON.stringify(true, logged));
