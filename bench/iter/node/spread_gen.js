function* range(n) {
  let i = 0;
  while (i < n) {
    yield i;
    i++;
  }
}
let sum = 0;
for (let round = 0; round < 20; round++) {
  const xs = [...range(5000000)];
  for (const x of xs) {
    sum += x;
    if (sum >= 1000000007) {
      sum -= 1000000007;
    }
  }
}
console.log(sum);
