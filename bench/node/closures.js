// Closures (same workload as closures.vlt).
function makeScaler(k) {
  return (x) => x * k + 1;
}

const xs = [];
for (let i = 0; i < 1000000; i++) {
  xs.push(i);
}
let total = 0;
for (let round = 0; round < 20; round++) {
  const k = round + 3;
  const m = (round % 5) + 2;
  total += xs
    .map((x) => x * k)
    .filter((x) => x % m === 0)
    .reduce((acc, x) => (acc + x) % 1000000007, 0);
  xs.forEach((x) => {
    total = (total + x * m) % 1000000007;
  });
}
console.log(total);

const scale = makeScaler(7);
let acc = 0;
for (let i = 0; i < 20000000; i++) {
  acc = scale(acc + i) % 1000003;
}
console.log(acc);
