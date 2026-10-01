// 10M sequential awaits of an async function that completes immediately.
async function step(x, i) {
  return (x * 31 + i) % 1000003;
}

async function main() {
  let acc = 1;
  for (let i = 0; i < 10000000; i++) {
    acc = await step(acc, i);
  }
  console.log(acc);
}
main();
