// Node twin of any_call_kept_hooks.velt: the same program with its types removed.
function check(x) {
  if (x < 0) throw new RangeError("negative");
  return x;
}
const table = { check, show: () => "shown" };
try {
  table.check(-1);
} catch (e) {
  console.log("caught", e.message);
}

class Shown {
  constructor(data) {
    this.data = data;
  }
  toString() {
    return this.data.show();
  }
}
console.log(`${new Shown(table)}`, String(new Shown(table)));

class Json {
  constructor(data) {
    this.data = data;
  }
  toJSON() {
    return { j: this.data.check(3) };
  }
}
console.log(JSON.stringify(new Json(table)));

class Safe {
  constructor(data) {
    this.data = data;
  }
  toString() {
    try {
      return String(this.data.check(-2));
    } catch (e) {
      return "safe: " + e.message;
    }
  }
}
console.log(`${new Safe(table)}`);

class Failed extends Error {
  constructor(data) {
    super();
    this.data = data;
  }
  get message() {
    return "failed: " + this.data.show();
  }
}
try {
  throw new Failed(table);
} catch (e) {
  console.log(e.message);
}

class Described {
  constructor(data) {
    this.data = data;
  }
  describe() {
    return "described " + this.data.show();
  }
  toString() {
    return this.describe();
  }
  toJSON() {
    return { d: this.describe() };
  }
}
const d = new Described(table);
console.log(`${d}`, JSON.stringify(d));
