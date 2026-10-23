// timer_await.js at the top level (run as an ES module: top-level await).
let s = 0;
for (let i = 0; i < 20000; i++) {
  s += await new Promise((r) => setTimeout(() => r(i % 7), 0));
}
console.log(s);
