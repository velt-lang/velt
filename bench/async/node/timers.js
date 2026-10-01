// 100k concurrent sleep(1) tasks, joined with Promise.all.
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function nap(i) {
  await sleep(1);
  return i % 13;
}

async function main() {
  const ps = [];
  for (let i = 0; i < 100000; i++) {
    ps.push(nap(i));
  }
  const rs = await Promise.all(ps);
  let total = 0;
  for (const r of rs) {
    total += r;
  }
  console.log(total);
}
main();
