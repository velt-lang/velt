// `String(v)` and template literals of `any` values of several kinds, 200,000 times each.
class P {
  toString(): string {
    return "p";
  }
}

const vals: any[] = [1, "s", true, { a: 1 }, [1, 2], new P(), new Map<string, number>(), null];
let total = 0;
for (let n = 0; n < 200000; n++) {
  const v = vals[n % vals.length];
  total += String(v).length + `${v}!`.length;
}
console.log(total);
