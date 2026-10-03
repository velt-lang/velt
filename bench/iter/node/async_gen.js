async function step(i) {
  return i;
}
async function* values(n) {
  let i = 0;
  while (i < n) {
    yield await step(i);
    i++;
  }
}
async function main() {
  let sum = 0;
  for (let round = 0; round < 20; round++) {
    for await (const x of values(3000000)) {
      sum += x;
      if (sum >= 1000000007) {
        sum -= 1000000007;
      }
    }
  }
  console.log(sum);
}
main();
