// 1000 rounds of Promise.all over 1000 small async tasks (JS promises start eagerly).
async function task(round, i) {
  return (round * 1000 + i) % 7919;
}

async function main() {
  let total = 0;
  for (let round = 0; round < 1000; round++) {
    const ps = [];
    for (let i = 0; i < 1000; i++) {
      ps.push(task(round, i));
    }
    const rs = await Promise.all(ps);
    for (const r of rs) {
      total += r;
    }
  }
  console.log(total);
}
main();
