// The Node twin of tests/golden/lang/proxy_callable_get.velt; its .out is what Node 22 prints.
const seen = [];
const traced = new Proxy((x) => x + 1, {
  get(t, k, r) {
    seen.push(String(k));
    return Reflect.get(t, k, r);
  },
  apply(t, th, a) {
    return "trap " + a.join(",");
  },
});
console.log(traced.call(null, 1), traced.apply(null, [2, 3]));
console.log(seen.join(","));
const o = { name: "o" };
const reads = [];
const named = new Proxy(function (x) { return this?.name + x; }, {
  get(t, k, r) {
    reads.push(String(k));
    return Reflect.get(t, k, r);
  },
});
console.log(named.call(o, 1), named.apply(o, [2]), named(4));
console.log(reads.join(","));
console.log(traced.bind(null, 4)(5), named.bind(o, 3)());
const plain = new Proxy(function (x) { return this?.name + x; }, {});
console.log(plain.call(o, 5), plain.apply(o, [6]), plain.bind(o)(7));
const inner = new Proxy((a, b) => a + b, {
  apply(t, th, a) {
    return "inner " + (th === undefined ? "-" : th.name) + " " + a.join(",");
  },
});
const outer = new Proxy(inner, {});
const b = outer.bind(o, 1);
console.log(b(2), outer.call(o, 3, 4), outer.apply(o, [5, 6]), typeof b);
