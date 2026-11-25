// A promise held as `any` cast to `Promise<number>` and awaited (#924 step 4): 200 rounds over
// 1,000 promises. Here each holds a `Promise<any>`, which the cast converts; promise_cast_exact.ts
// holds `Promise<number>`s, and promise_cast_typed.ts awaits them typed, without `any`.
async function main(): Promise<void> {
  const held: any[] = [];
  for (let i = 0; i < 1000; i++) {
    const p: Promise<any> = Promise.resolve(i);
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
