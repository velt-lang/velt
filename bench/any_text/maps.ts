// `String(v)` of an `any` holding a `Map`, 200,000 times.
const vals: any[] = [];
for (let i = 0; i < 100; i++) {
  vals.push(new Map<string, number>([["k", i]]));
}
let total = 0;
for (let n = 0; n < 200000; n++) {
  total += String(vals[n % 100]).length;
}
console.log(total);
