// Closures held in locals (same workload as closures_held.vlt).
const words = ["alpha", "beta", "gamma", "delta"];
let total = 0;
for (let i = 0; i < 3000000; i++) {
  const k = (i % 7) + 1;
  const m = i % 3;
  const scale = (x) => x * k + m;
  const w = words[i % 4];
  const score = (n) => n + w.length * k;
  total = (total + scale(i) + scale(i + 1) + score(i)) % 1000000007;
}
console.log(total);
