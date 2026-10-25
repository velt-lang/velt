// A hot loop of `==` on `any` values holding only numbers (#924), for instruction counts.
function eq(a: any, b: any): boolean {
  return a == b;
}
const xs: any[] = [];
for (let i = 0; i < 100000; i++) {
  xs.push(i % 5);
}
let n = 0;
for (let i = 0; i + 1 < xs.length; i++) {
  if (eq(xs[i], xs[i + 1])) {
    n++;
  }
  if (xs[i] == 3) {
    n++;
  }
}
console.log(n);
