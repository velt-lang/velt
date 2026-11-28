// Node twin of any_call_caught_callbacks.velt: the same program with its types removed.
class Calls {
  constructor(f) {
    this.f = f;
  }
  run(n) {
    return this.f(n);
  }
}

function main() {
  const f = (x) => {
    if (x === 2) {
      throw new RangeError("x " + x);
    }
    return x * 10;
  };
  [1, 3].forEach((n) => console.log("each", f(n)));
  setTimeout(() => console.log("timer", f(1)), 0);
  const r = new Calls(f);
  console.log("runner", r.run(1));
  try {
    r.run(2);
  } catch (e) {
    console.log("runner caught", `${e}`);
  }
  const m = new Map([["a", 1]]);
  m.forEach((v, k) => console.log("map", k, f(v)));
  const s = [3, 1].map((n) => f(n));
  console.log(s);
  try {
    [1, 2, 3].forEach((n) => console.log("visit", f(n)));
  } catch (e) {
    console.log("forEach caught", `${e}`);
  }
  try {
    console.log([3, 2].map((n) => f(n)));
  } catch (e) {
    console.log("map caught", `${e}`);
  }
  try {
    m.set("b", 2);
    m.forEach((v, k) => console.log("map", k, f(v)));
  } catch (e) {
    console.log("Map.forEach caught", `${e}`);
  }
  console.log("after", [1, 3].map((n) => f(n)));
}

main();
