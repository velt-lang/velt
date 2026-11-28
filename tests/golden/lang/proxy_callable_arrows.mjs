// The Node twin of tests/golden/lang/proxy_callable_arrows.velt; its .out is what Node 22 prints.
const log = [];
const add = (a, b) => a + b;
const traced = new Proxy(add, {
  apply(target, thisArg, args) {
    log.push(`${thisArg === undefined ? "undefined" : thisArg.name}(${args.join(",")})`);
    return target(args[0], args[1]) * 10;
  },
});
const o = { name: "o", t: traced };
console.log(traced(1, 2), o.t(3, 4));
console.log(log.join(" "));
const plain = new Proxy(add, {});
const arr = [plain, traced];
console.log(plain(2, 3), arr[0](4, 5), arr[1](1, 1), typeof plain, typeof traced);
const typed = new Proxy(add, {});
console.log(typed(10, 20));
const r = { name: "r" };
const viaGet = new Proxy(r, {
  get(t, k, recv) {
    return k === "run" ? traced : Reflect.get(t, k, recv);
  },
});
console.log(viaGet.run(2, 2), log[log.length - 1]);
let n = 0;
const counter = new Proxy(() => ++n, {});
counter();
counter();
console.log(n, [1, 2, 3].map((x) => plain(x, x)).join(","));
const sum = new Proxy((...xs) => xs.reduce((s, x) => s + x, 0), {});
console.log(sum(), sum(1, 2, 3, 4));
