// 1000 rounds of Promise.allSettled over 1000 small async tasks, every 7th of which rejects.
class Failed extends Error {}

async function task(round, i) {
  if (i % 7 == 3) {
    throw new Failed("odd one");
  }
  return (round * 1000 + i) % 7919;
}

async function main() {
  let total = 0;
  let failed = 0;
  for (let round = 0; round < 1000; round++) {
    const ps = [];
    for (let i = 0; i < 1000; i++) {
      ps.push(task(round, i));
    }
    const rs = await Promise.allSettled(ps);
    for (const r of rs) {
      if (r.status == "fulfilled") {
        total += r.value;
      } else {
        failed += 1;
      }
    }
  }
  console.log(total, failed);
}
main();
