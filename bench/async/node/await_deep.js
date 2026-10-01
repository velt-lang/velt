// A recursive async function (depth 20) awaited 500k times.
async function deep(n, x) {
  if (n === 0) {
    return x;
  }
  const r = await deep(n - 1, x + n);
  return (r * 7 + 1) % 1000003;
}

async function main() {
  let acc = 0;
  for (let i = 0; i < 500000; i++) {
    acc = (acc + (await deep(20, i))) % 1000003;
  }
  console.log(acc);
}
main();
