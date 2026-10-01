// An await-free hot loop inside an async function (see hot_loop.vlt).
async function seed(round) {
  return (round * 7919 + 17) % 65537;
}

async function main() {
  let total = 0;
  let kept = 0;
  let bytes = [];
  for (let round = 0; round < 40; round++) {
    let x = await seed(round);
    for (let i = 0; i < 5000000; i++) {
      x = (x + i * 7) & 65535;
      const b = x & 255;
      if (b != 10) {
        bytes.push(b);
      }
      total += b;
    }
    kept += bytes.length;
    bytes = [];
  }
  console.log(`${total} ${kept}`);
}
main();
