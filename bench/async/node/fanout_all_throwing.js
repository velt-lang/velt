// 1M rounds of `await Promise.all([a(), b()])` over two small async functions that can throw
// (they never do).
async function task(round, i) {
  if (round < 0) {
    throw new Error("never");
  }
  return (round * 1000 + i) % 7919;
}

async function main() {
  let total = 0;
  for (let round = 0; round < 1000000; round++) {
    const rs = await Promise.all([task(round, 1), task(round, 2)]);
    total += rs[0] + rs[1];
  }
  console.log(total);
}
main();
