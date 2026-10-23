// 1M sequential top-level awaits of a resolved promise (run as an ES module: top-level await).
let s = 0;
for (let i = 0; i < 1000000; i++) {
  s += await Promise.resolve(i % 7);
}
console.log(s);
