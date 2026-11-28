// The Node twin of tests/golden/lang/proxy_callable.velt; its .out is what Node 22 prints.
const m = function (x) {
  return this?.name + ":" + x;
};
const pm = new Proxy(m, {});
const o = { name: "o", k: pm };
console.log(typeof pm, pm(2), o.k(1), o["k"](3));
const log = [];
const add = (a, b) => a + b;
const traced = new Proxy(add, {
  apply(target, thisArg, args) {
    log.push(`${thisArg === undefined ? "undefined" : thisArg.name}(${args.join(",")})`);
    return target(args[0], args[1]) * 10;
  },
});
const ot = { name: "ot", t: traced };
const key = "t";
console.log(traced(1, 2), ot.t(3, 4), ot[key](5, 6));
console.log(log.join(" "));
const inner = new Proxy(m, {
  apply(t, th, a) {
    return "inner " + th?.name + " " + a[0];
  },
});
const nested = new Proxy(inner, {});
const o2 = { name: "o2", n: nested };
console.log(nested(1), o2.n(2), typeof nested);
const outer = new Proxy(nested, {
  apply(t, th, a) {
    return "outer " + t(a[0]);
  },
});
console.log(outer(3));
const trapped = new Proxy(m, {
  apply(t, th, a) {
    return "trap " + th?.name + " " + a.join(",");
  },
});
const self = { name: "self" };
console.log(trapped.call(self, 3), trapped.apply(self, [4, 5]), trapped.apply(self));
const b = trapped.bind(self, 6);
console.log(b(7), b());
console.log(pm.call(self, 8), pm.apply(self, [9]));
const obj = new Proxy({}, {});
try {
  obj(1);
} catch (e) {
  console.log(e instanceof TypeError, e.message);
}
const objTrap = new Proxy({ a: 1 }, {
  apply(t, th, a) {
    return 1;
  },
});
try {
  objTrap(1);
} catch (e) {
  console.log(e instanceof TypeError, e.message);
}
console.log(typeof obj, typeof objTrap, typeof outer, typeof trapped);
function named(x) {
  return x;
}
const pn = new Proxy(named, {});
console.log(pn, [pn], pn(4));
