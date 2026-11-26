// Node twin of override_fewer_params.velt: the same program with its types removed.
// An override, or a method implementing an interface, may take fewer parameters than the
// method it stands for, as in TypeScript: it ignores the arguments after its own (#3819). A
// call through the base class or the interface passes them all; a call on the class itself
// passes only its own.
class Ctx {
  id;
  constructor(id) {
    this.id = id;
  }
}

class Base {
  n = 0;
  run(s, from) {
    return s + (from ?? 0);
  }
  step(s, by = 1) {
    return s + by;
  }
  note(msg, level) {
    console.log("base note", msg, level);
  }
  bump(by, twice) {
    this.n += twice ? by * 2 : by;
    return this.n;
  }
  zero(a, b) {
    return "base zero " + (a + b);
  }
  // No return types written: both results are inferred.
  plain(s, ctx) {
    return "base plain " + s + ctx.id;
  }
}

class Short extends Base {
  run(s) {
    return "short " + s;
  }
  step(s) {
    return "short step " + s;
  }
  note(msg) {
    console.log("short note", msg);
  }
  bump(by) {
    this.n += by + 100;
    return this.n;
  }
  zero(a) {
    return "short zero " + a;
  }
  plain(s) {
    return "short plain " + s;
  }
}

// Fewer again, and an override of the shorter method with the same parameters.
class Shorter extends Short {
  zero() {
    return "shorter zero";
  }
  run(s) {
    return "shorter " + super.run(s);
  }
}

const all = [new Base(), new Short(), new Shorter()];
for (const b of all) {
  console.log(b.run("a"), b.run("a", 3), b.step("s"), b.step("s", 5));
  b.note("m", 2);
  const one = b.bump(1);
  const two = b.bump(1, true);
  console.log(one, two, b.n);
  console.log(b.zero(1, 2), b.plain("p", new Ctx(7)));
}
const shorts = [new Short(), new Shorter()];
for (const s of shorts) {
  console.log(s.run("b"), s.zero(5), s.bump(2), s.plain("q"), s.step("t"));
  s.note("n");
}
console.log(new Shorter().zero());

// Beside an override that gives the dropped parameter another default.
class Stepper {
  step(s, by = 1) {
    return "stepper " + s + by;
  }
}

class Twice extends Stepper {
  step(s, by = 2) {
    return "twice " + s + by;
  }
}

class Still extends Stepper {
  step(s) {
    return "still " + s;
  }
}

class StillTwice extends Twice {
  step(s) {
    return "still twice " + s;
  }
}

const steppers = [new Stepper(), new Twice(), new Still(), new StillTwice()];
for (const b of steppers) {
  console.log(b.step("a"), b.step("b", 7));
}
const twice = new StillTwice();
console.log(twice.step("c"), twice.step("d", 9), new StillTwice().step("f"));

// A rest parameter of the base method is one of those left out.
class Joiner {
  all(a, ...rest) {
    return a + rest.length;
  }
  log(...parts) {
    return parts.join("+");
  }
}

class Terse extends Joiner {
  all(a) {
    return a * 10;
  }
  log() {
    return "none";
  }
}

const joiners = [new Joiner(), new Terse()];
for (const j of joiners) {
  console.log(j.all(1), j.all(1, 2, 3), j.log("x", "y"), new Terse().all(4), new Terse().log());
}

// A method that returns `this`.
class Chain {
  n = 0;
  add(a, b) {
    this.n += a + b;
    return this;
  }
}

class One extends Chain {
  add(a) {
    this.n += a * 100;
    return this;
  }
}

const chains = [new Chain(), new One()];
for (const c of chains) {
  console.log(c.add(1, 2).add(3, 4).n);
}
console.log(new One().add(5).add(6).n);

// An abstract method, implemented with fewer parameters.
class Shape {
  describe() {
    return "shape " + this.area(2, "cm");
  }
}

class Square extends Shape {
  area(scale) {
    return "square " + scale * scale;
  }
}

class Dot extends Square {
  area() {
    return "dot";
  }
}

const shapes = [new Square(), new Dot()];
for (const s of shapes) {
  console.log(s.area(3, "m"), s.describe());
}
console.log(new Square().area(4), new Dot().area());

// A generic base class.
class Box {
  items = [];
  put(v, tag) {
    this.items.push(v);
    return this.items.length;
  }
  label(v, ctx) {
    return "box " + ctx.id;
  }
}

class NumBox extends Box {
  put(v) {
    this.items.push(v * 2);
    return -this.items.length;
  }
  label(v) {
    return "num " + v;
  }
}

class AnyBox extends Box {
  put(v) {
    return 42;
  }
}

const nums = new NumBox();
const p1 = nums.put(1);
const p2 = nums.put(2, "t");
console.log(p1, p2, nums.items, nums.label(3, new Ctx(9)));
const strs = new AnyBox ();
console.log(strs.put("x", "t"), strs.label("y", new Ctx(4)), new AnyBox ().put(true));

// Through `any`, where the arguments are whatever the call passes.
const dynamic = new Short();
console.log(dynamic.run("q", 5), dynamic.run("r"));

// Interface methods: implemented by the class itself, by a method it inherits, by an override,
// and by a generic class.

class Own {
  run(s) {
    return "own " + s;
  }
  note() {
    console.log("own note");
  }
}

class Counter {
  n = 0;
  run(s) {
    this.n += 1;
    return "counter " + s + this.n;
  }
  note(msg) {
    console.log("counter note", msg);
  }
}

class Inherits extends Counter {}

class Overrides extends Counter {
  run(s) {
    return "over " + s;
  }
  async load(id) {
    return "loaded " + id;
  }
}

class Gen {
  v;
  constructor(v) {
    this.v = v;
  }
  run(s) {
    return "gen " + s + this.v.length;
  }
  note(msg) {
    console.log("gen note", msg);
  }
}

// An interface method with a rest parameter, implemented without it.

class Quiet {
  write(tag) {
    return "quiet " + tag;
  }
  all() {
    return -1;
  }
}

class Full {
  write(tag, ...parts) {
    return tag + parts.length;
  }
  all(...parts) {
    return parts.length;
  }
}

const sinks = [new Quiet(), new Full()];
for (const s of sinks) {
  console.log(s.write("t"), s.write("t", "a", "b"), s.all(), s.all(1, 2, 3));
}
console.log(new Quiet().write("q"), new Quiet().all());

// Async methods.
class Fetcher {
  async load(id, opts) {
    return "base " + id + (opts ? String(opts.retries) : "");
  }
}

class Fast extends Fetcher {
  async load(id) {
    return "fast " + id;
  }
}

async function main() {
  const runners = [new Own(), new Inherits(), new Overrides(), new Gen ([1, 2])];
  for (const r of runners) {
    console.log(r.run("a"), r.run("b", 2));
    r.note("m", 3);
  }
  const l = new Overrides();
  console.log(await l.load("x", 2));
  const c = new Overrides();
  console.log(c.run("c"), new Own().run("d"));
  const fetchers = [new Fetcher(), new Fast()];
  for (const f of fetchers) {
    console.log(await f.load("a"), await f.load("b", { retries: 2 }));
  }
  console.log(await new Fast().load("c"));
}

main();
