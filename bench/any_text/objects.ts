// `String(v)` of an `any` holding an object literal, 200,000 times (`__anyText`).
const vals: any[] = [];
for (let i = 0; i < 100; i++) {
  vals.push({ i });
}
let total = 0;
for (let n = 0; n < 200000; n++) {
  total += String(vals[n % 100]).length;
}
console.log(total);
