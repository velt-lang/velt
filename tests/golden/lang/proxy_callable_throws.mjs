// The Node twin of tests/golden/lang/proxy_callable_throws.velt; its .out is what Node 22 prints.
class Boom extends Error {}
const f = (x) => x * 2;
const thrower = new Proxy(f, {
  apply(t, th, a) {
    if (a[0] > 1) {
      throw new Boom("boom " + a[0]);
    }
    return t(a[0]);
  },
});
const nested = new Proxy(thrower, {});
function callIt(g, x) {
  return g(x);
}
try {
  console.log(thrower(1));
  console.log(nested(2));
  console.log("not reached");
} catch (e) {
  console.log("nested", e instanceof Boom, e.message);
}
try {
  callIt(thrower, 3);
} catch (e) {
  console.log("through a function", e instanceof Boom, e.message);
} finally {
  console.log("finally");
}
const typeErr = new Proxy(f, {
  apply(t, th, a) {
    throw new TypeError("no " + a.length);
  },
});
try {
  typeErr(1, 2);
} catch (e) {
  console.log("type error", e instanceof TypeError, e.message);
}
const plain = (x) => x + 1;
console.log(plain(1), callIt(plain, 2), callIt(nested, 1));
