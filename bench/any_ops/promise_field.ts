// A promise field read through `any` and awaited (#924 step 3): 200 rounds over 1,000 objects.
// promise_field_typed.ts is the same loop on the typed field.
class Job {
  p: Promise<number>;
  constructor(n: number) {
    this.p = Promise.resolve(n);
  }
}

async function main(): Promise<void> {
  const jobs: any[] = [];
  for (let i = 0; i < 1000; i++) {
    jobs.push(new Job(i));
  }
  let sum = 0;
  for (let r = 0; r < 200; r++) {
    for (const j of jobs) {
      sum += await j.p;
    }
  }
  console.log(sum);
}
main();
