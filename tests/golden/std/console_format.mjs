// The Node twin of tests/golden/std/console_format.velt: `console.log`'s format specifiers, as
// Node's `util.format` reads them (#3662).
class Point {
  constructor(x, y) {
    this.x = x;
    this.y = y;
  }
  toString() {
    return `(${this.x}, ${this.y})`;
  }
}

class Plain {
  constructor() {
    this.a = 1;
    this.inner = { b: 2 };
  }
}

class Shout extends Error {
  toString() {
    return `Shout! ${this.message}`;
  }
}

function label() {
  return "%s has %d items";
}

const nested = { a: { b: { c: 1 } }, list: [1, [2, [3]]] };

// %s
console.log("%s!", "hi");
console.log("%s|%s|%s|%s", 1.5, -0, NaN, Infinity);
console.log("%s|%s|%s", true, null, undefined);
console.log("%s", Symbol("k"));
console.log("%s", nested);
console.log("%s", [1, [2, [3]]]);
console.log("%s|%s", new Point(1, 2), new Plain());
console.log("%s", new URL("https://example.com/a?b=1"));
console.log("%s", new Date(0));
console.log("%s", new Shout("loud"));
console.log("%s", new Map([["k", { v: 1 }]]));

// %d %i %f
console.log("%d|%d|%d|%d", 42, -0, 1.5, NaN);
console.log("%d|%d|%d|%d", "12", "12px", true, null);
console.log("%d|%d|%d", undefined, {}, [5]);
console.log("%d|%i|%f", Symbol("s"), Symbol("s"), Symbol("s"));
console.log("%i|%i|%i|%i", 3.9, -3.9, "42.5abc", -0);
console.log("%i|%i", 1e21, "0x1f");
console.log("%f|%f|%f|%f", 3.25, "1e3x", -0, "abc");
console.log("%f|%f", Infinity, true);

// %j
console.log("%j", { a: 1, b: [true, null, "x"] });
console.log("%j|%j|%j", "str", 5, [1, 2]);
console.log("%j", new Point(3, 4));

// %o %O
console.log("%O", nested);
console.log("%O|%O|%O", "quoted", 7, -0);
console.log("%o", 42);

// %c
console.log("%cstyled%c text", "color: red", "font-weight: bold");

// %%
console.log("100%%");
console.log("100%% of %s", "it");
console.log("%d%%", 50);

// missing arguments
console.log("%s and %s", "one");
console.log("%d-%d-%d", 1, 2);

// excess arguments
console.log("%s", "a", "b", 3, { c: 4 });
console.log("x%sy", 1, [2], "three");

// unknown specifiers and a trailing %
console.log("%x %s %y", "v");
console.log("50%", 1);
console.log("%q", "a");
console.log("%s%", "a");

// a non-string first argument
console.log(5, "%s", "x");
console.log({ a: 1 }, "%d", 2);

// a string holding a `%` as an argument
const anything = new Point(5, 6);
console.log("[%s] [%s]", label(), anything);

// arguments held by `any`
const dyn = [new Point(7, 8), { k: { deep: 1 } }, -0, "12px", [1, [2]]];
console.log("%s|%s|%s|%s|%s", dyn[0], dyn[1], dyn[2], dyn[3], dyn[4]);
console.log("%d|%i|%f|%j|%O|%i", dyn[3], dyn[2], dyn[0], dyn[1], dyn[3], dyn[3]);

// numbers, strings and booleans for every specifier
const nums = [-0.5, -1e21, 5e-7, -5e-7, 1e21, Infinity, -0, 2.5, NaN];
for (const n of nums) {
  console.log("%s|%d|%i|%f|%j|%o|%O|%c|", n, n, n, n, n, n, n, n);
}
const strs = ["12.5px", " 42 ", "", "0x1f", "a\"b\\c\nd\u0001", "-0"];
for (const s of strs) {
  console.log("%s|%d|%i|%f|%j|%o|%O|%c|", s, s, s, s, s, s, s, s);
}
for (const b of [true, false]) {
  console.log("%s|%d|%i|%f|%j|%o|%O|%c|", b, b, b, b, b, b, b, b);
}

// every argument is evaluated before any is converted
let log = [];
class Loud {
  constructor(name) {
    this.name = name;
  }
  toString() {
    log.push("toString " + this.name);
    return "L" + this.name;
  }
  valueOf() {
    log.push("valueOf " + this.name);
    return 7;
  }
  toJSON() {
    log.push("toJSON " + this.name);
    return "J" + this.name;
  }
}
function mk(n) {
  log.push("eval " + n);
  return new Loud(n);
}
function num(n) {
  log.push("evalnum " + n);
  return n;
}
console.log("%s %s", mk("a"), mk("b"));
console.log(log.join(", "));
log = [];
console.log("%j %s %d", mk("a"), mk("b"), num(3));
console.log(log.join(", "));
log = [];
console.log("%c%%%s", mk("a"), mk("b"), mk("c"));
console.log(log.join(", "));
log = [];
console.log("%d", mk("a"), mk("b"));
console.log(log.join(", "));

// `%s` of a class with its own `toString()` behind a nullable, a union, an interface, a type
// parameter, and through `any`
class Tag {
  toString() {
    return "TAG";
  }
}
class Sq {
  area() {
    return 4;
  }
  toString() {
    return "SQ";
  }
}
function viaShape(x) {
  console.log("%s", x);
}
function viaNull(p) {
  console.log("%s", p);
}
function viaUnion(p) {
  console.log("%s", p);
}
function viaUndefined(p) {
  console.log("%s", p);
}
function viaInterface(n) {
  console.log("%s", n);
}
function viaParam(x) {
  console.log("%s|%j", x, x);
}
viaNull(new Point(1, 1));
viaNull(null);
viaUnion(new Point(2, 2));
viaUnion("text");
viaUndefined(new Point(3, 3));
viaInterface(new Point(4, 4));
viaInterface(new Tag());
viaShape(new Sq());
viaParam(new Point(5, 5));
viaParam("seven");
const loudAny = new Shout("any");
console.log("%s", loudAny);
const symAny = Symbol("q");
console.log("%d|%i|%f", symAny, symAny, symAny);
console.log("%s", new Uint8Array([1, 2]));

// a program's own `JSON` does not hide `%j`'s
function ownJson() {
  const JSON = 5;
  console.log("%j|%d", { a: 1 }, JSON);
}
ownJson();

// a `BigInt` as Node writes a bigint
const big = 123n;
console.log("%s|%d|%i|%f", big, big, big, big);

// `%o` and `%O` of a `Date`, also behind a nullable and an `any`
const day = new Date(86400000);
const maybeDay = day;
const anyDay = day;
console.log("%o|%O|%O|%O", day, day, maybeDay, anyDay);

// `%j` of what `JSON.stringify` gives `undefined` for, and of a nullable
const noNumber = undefined;
const someNumber = 5;
function optional(x) {
  console.log("%j|%j", x, x);
}
console.log("%j|%j|%j|%j", noNumber, someNumber, Symbol("s"), (n) => n);
optional();
optional(7);
const anyUndefined = undefined;
const anySymbol = Symbol("t");
console.log("%j|%j|%j", anyUndefined, anySymbol, anyNumber());
function anyNumber() {
  return 3;
}
const anySet = new Set([2]);
console.log("%j|%j|%j", new Set([1]), anySet, new Map([["k", 1]]).size);
function jsonShape(x) {
  console.log("%j", x);
}
jsonShape(new Sq());

// `%s` of a class with `[Symbol.toPrimitive]`, typed and through `any`
class Prim {
  [Symbol.toPrimitive](hint) {
    return hint === "number" ? 11 : "prim";
  }
}
const prim = new Prim();
const anyPrim = new Prim();
console.log("%s|%s|%d", prim, anyPrim, prim);

// `%s` of a class with its own `toString()` and a custom inspect: the `toString()`
const custom = Symbol.for("nodejs.util.inspect.custom");
class Both {
  toString() {
    return "both-text";
  }
  [custom]() {
    return "both-inspect";
  }
}
console.log("%s|%O", new Both(), new Both());

// non-ASCII
console.log("é%sü → %d€", "ñ", 3);
console.log("日本%s語", "の");
console.log("%s", "🙂", "😀");

// console.error and the other levels
console.error("err %s %d", "x", 1);
console.warn("warn %s", "y");
console.info("info %i", 2.5);
console.debug("debug %%");
