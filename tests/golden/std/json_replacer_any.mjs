// Node twin of json_replacer_any.velt: prints its .out.

class Money {
  constructor(cents) {
    this.cents = cents;
  }
  toJSON(key) {
    return `${key}:${this.cents}`;
  }
}

function literal(k, v) {
  if (k === "a") return { z: [9], m: new Money(5) };
  return v;
}

function root(k, v) {
  if (k === "") return [1, { b: 2 }, "s"];
  return v;
}

function scalars(k, v) {
  if (k === "n") return 42;
  if (k === "s") return "str";
  if (k === "b") return false;
  if (k === "u") return undefined;
  if (k === "x") return null;
  if (k === "f") return () => 1;
  return v;
}

function seen(k, v) {
  console.log(`  replacer ${JSON.stringify(k)}`);
  return v;
}

console.log(JSON.stringify({ a: 1, b: { c: 2 } }, literal));
console.log(JSON.stringify({ a: 1 }, root));
console.log(JSON.stringify({ n: 1, s: 1, b: 1, u: 1, x: 1, f: 1, k: 1 }, scalars));
console.log(JSON.stringify([1, 2, 3], (k, v) => v));
console.log(JSON.stringify({ a: [true], m: new Money(7) }, seen));
