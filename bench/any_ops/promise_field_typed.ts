// promise_field.ts's loop on the typed field: what reading through `any` adds to it.
class Job {
  p: Promise<number>;
  constructor(n: number) {
    this.p = Promise.resolve(n);
  }
}

async function main(): Promise<void> {
  const jobs: Job[] = [];
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
