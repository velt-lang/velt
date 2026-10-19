// The Node twin's per-request cost on one core, without HTTP (inprocess.vlt does the same).
//   node node/inprocess.mjs
import { Store, route } from "./server.mjs";

let t = performance.now();
const s = Store.seeded(42, 5000, 20000);
console.log(`seed 5000 products + 20000 orders: ${(performance.now() - t).toFixed(1)} ms`);
const cases = [
  ["stats", "/stats", "", 200],
  ["list 100, sorted by price", "/products", "limit=100&sort=price", 200],
  ["list 100 + serialize", "/products", "limit=100", 2000],
  ["product by id", "/products/123", "", 20000],
];
for (const [label, path, query, n] of cases) {
  for (let i = 0; i < 20; i++) route(s, "GET", path, query, "");
  t = performance.now();
  for (let i = 0; i < n; i++) route(s, "GET", path, query, "");
  console.log(`${label}: ${((performance.now() - t) / n).toFixed(4)} ms`);
}
