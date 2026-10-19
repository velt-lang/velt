// A closure created per iteration and only called (same workload as closures_borrowed.vlt).
function score(name, base, scale) {
  const weigh = (k) => {
    const n = name.length + k;
    return (base * scale + n) % 9973;
  };
  return weigh(1) + weigh(2);
}

const names = ["ada", "grace", "linus", "barbara", "ken"];
let total = 0;
for (let i = 0; i < 3000000; i++) {
  total = (total + score(names[i % 5], i % 1000, (i % 13) + 1)) % 1000003;
}
console.log(total);
