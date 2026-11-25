// The Node twin of tests/golden/std/json_replacer_tojson_order.velt. `view` prints what the
// replacer got as Velt's `JsonValue.stringify()` prints its view: the value as JSON with no
// `toJSON` called (a `Date` has no enumerable own properties: `{}`).

function view(v) {
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

class Inner {
  c = new Hook("c");
  d = [new Hook("d0"), new Hook("d1")];
}

class Outer {
  a = new Hook("a");
  inner = new Inner();
  b = new Hook("b");
}

class Top {
  toJSON(key) {
    console.log(`  toJSON top key=${JSON.stringify(key)}`);
    return new Outer();
  }
}

class Self {
  x = 1;
  h = new Hook("h");
  toJSON() {
    console.log("  toJSON self");
    return this;
  }
}

class Dated {
  label = "d";
  when = new Date(0);
}

function logged(k, v) {
  console.log(`  replacer ${JSON.stringify(k)} ${view(v)}`);
  return v;
}

console.log("nested:");
console.log(JSON.stringify(new Outer(), logged));
console.log("root toJSON:");
console.log(JSON.stringify(new Top(), logged));
console.log("toJSON returning this:");
console.log(JSON.stringify(new Self(), logged));
console.log("array:");
console.log(JSON.stringify([new Hook("p"), new Hook("q")], logged));
console.log("date:");
console.log(JSON.stringify(new Dated(), logged));
console.log("filter:");
const drop = (k, v) => {
  console.log(`  replacer ${JSON.stringify(k)}`);
  return k == "b" || k == "inner" ? undefined : v;
};
console.log(JSON.stringify(new Outer(), drop));
console.log("replaced:");
const swap = (k, v) => {
  console.log(`  replacer ${JSON.stringify(k)} ${view(v)}`);
  return k == "inner" ? JSON.parse(`{"z":[1]}`) : v;
};
console.log(JSON.stringify(new Outer(), swap));
console.log("any:");
const o = new Outer();
console.log(JSON.stringify(o, logged));
console.log("spaced:");
console.log(JSON.stringify([new Hook("s")], logged, 1));
