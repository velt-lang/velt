let sum = 0;
for (let round = 0; round < 20; round++) {
  const xs = [];
  let i = 0;
  while (i < 5000000) {
    xs.push(i);
    i++;
  }
  for (const x of xs) {
    sum += x;
    if (sum >= 1000000007) {
      sum -= 1000000007;
    }
  }
}
console.log(sum);
