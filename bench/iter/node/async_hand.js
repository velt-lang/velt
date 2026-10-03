async function step(i) {
  return i;
}
async function main() {
  let sum = 0;
  for (let round = 0; round < 20; round++) {
    let i = 0;
    while (i < 3000000) {
      sum += await step(i);
      if (sum >= 1000000007) {
        sum -= 1000000007;
      }
      i++;
    }
  }
  console.log(sum);
}
main();
