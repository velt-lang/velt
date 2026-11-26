// promise_cast.ts's loop on typed promises, without `any`: what the cast adds to an await.
async function main(): Promise<void> {
  const held: Promise<number>[] = [];
  for (let i = 0; i < 1000; i++) {
    held.push(Promise.resolve(i));
  }
  let sum = 0;
  for (let r = 0; r < 200; r++) {
    for (const p of held) {
      sum += await p;
    }
  }
  console.log(sum);
}
main();
