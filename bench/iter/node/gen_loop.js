function* range(n) {
  let i = 0;
  while (i < n) {
    yield i;
    i++;
  }
}
let sum = 0;
for (let round = 0; round < 20; round++) {
  for (const x of range(30000000)) {
    sum += x;
    if (sum >= 1000000007) {
      sum -= 1000000007;
    }
  }
}
console.log(sum);
