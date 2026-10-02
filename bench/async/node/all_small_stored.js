// 1M rounds of `Promise.all` over a stored array of three small async calls that can throw
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
    const ps = [task(round, 1), task(round, 2), task(round, 3)];
    const rs = await Promise.all(ps);
    total += rs[0] + rs[1] + rs[2];
  }
  console.log(total);
}
main();
