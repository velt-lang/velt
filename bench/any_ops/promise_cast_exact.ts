// promise_cast.ts's loop on promises of the type the cast names (`Promise<number>`).
async function main(): Promise<void> {
  const held: any[] = [];
  for (let i = 0; i < 1000; i++) {
    const p: Promise<number> = Promise.resolve(i);
    held.push(p);
  }
  let sum = 0;
  for (let r = 0; r < 200; r++) {
    for (const p of held) {
      sum += await (p as Promise<number>);
    }
  }
  console.log(sum);
}
main();
