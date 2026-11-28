// The Node twin of tests/golden/lang/class_call_through_any.velt; its .out is what Node 22 prints.
class C {
  constructor(x) {
    this.x = x;
  }
}
class D extends C {
  static n = 1;
}
function use(f) {
  try {
    return f(2);
  } catch (e) {
    return "use " + (e instanceof TypeError) + " " + e.message;
  }
}
const pc = new Proxy(C, {});
try {
  console.log(use(pc));
} catch (e) {
  console.log("in fn", e instanceof TypeError, e.message);
}
const ck = C;
const o = { k: ck };
try {
  o.k(3);
} catch (e) {
  console.log("member", e instanceof TypeError, e.message);
}
const pd = D;
try {
  pd();
} catch (e) {
  console.log("statics", e instanceof TypeError, e.message);
}
const N = Number;
console.log(N("5") + 1);
console.log(typeof pc, typeof o.k);
