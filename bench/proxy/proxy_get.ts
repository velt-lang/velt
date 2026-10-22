// Cost of a proxied read: N reads through a `get` trap forwarding with Reflect.get, N writes
// through a `set` trap, and N plain reads of the same field.
function main(): void {
  const raw: any = { count: 1 };
  const p: any = new Proxy(raw, {
    get(obj, prop, receiver) {
      return Reflect.get(obj, prop);
    },
    set(obj, prop, v) {
      return Reflect.set(obj, prop, v);
    },
  });
  const n = process.argv.length > 2 ? Number(process.argv[2]) : 100000;
  let sum = 0;
  if (process.argv.length > 3 && process.argv[3] === "plain") {
    for (let i = 0; i < n; i++) {
      sum += raw.count as number;
    }
  } else {
    for (let i = 0; i < n; i++) {
      sum += p.count as number;
      p.count = i & 3;
    }
  }
  console.log(sum);
}
