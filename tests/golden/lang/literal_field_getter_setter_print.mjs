// Node twin of literal_field_getter_setter_print.velt: the same program with its types removed.
let store = 1;
const gs = {
  get v() {
    return store;
  },
  set v(x) {
    store = x * 10;
  },
  w: 2,
  show() {
    return "s" + store;
  },
};
console.log(gs);
const a = gs;
console.log(a);
a.v = 3;
console.log(store, gs.v, gs.show());
gs.v = 4;
console.log(store, gs);
console.log(JSON.stringify(a), Object.keys(a).join(","));
console.log({ inner: gs, n: 1 });
console.log([gs]);

let held = "x";
const p = {
  get a() {
    return held;
  },
  set a(x) {
    held = x + "!";
  },
  b: 1,
};
console.log(p);
p.a = "y";
const pa = p;
console.log(held, p.a, JSON.stringify(pa));

const og = {
  get g() {
    return 4;
  },
  tag() {
    return "t";
  },
};
console.log(og, og.g);
