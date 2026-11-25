// The Node twin of tests/golden/std/console_format_any.velt.
class Pt {
  constructor() {
    this.x = 1;
  }
  toString() {
    return "PT";
  }
}

const e = new Error("e");
console.log("%s", e.message);
const p = new Pt();
console.log("%s|%d", p, p);

let calls = 0;
class Same extends Error {
  toString() {
    calls++;
    return "Error: same";
  }
}
class Stacked extends Error {
  get stack() {
    return "Error: stacked";
  }
  toString() {
    return "Error: stacked";
  }
}
const same = new Same("same");
const stacked = new Stacked("s");
console.log("%s|%s", same, stacked);
console.log("calls " + calls);
