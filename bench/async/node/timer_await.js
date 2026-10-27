// 20k sequential awaits of a promise a 1 ms timer settles, in an async main.
async function main() {
  let s = 0;
  for (let i = 0; i < 20000; i++) {
    s += await new Promise((r) => setTimeout(() => r(i % 7), 0));
  }
  console.log(s);
}
main();
