// A hot loop of `x == null` on `any` values holding objects and numbers (#924), for
// instruction counts: the object-against-primitive arm is left out.
const xs: any[] = [];
for (let i = 0; i < 100000; i++) {
  xs.push(i % 3 === 0 ? { a: i } : i % 4 === 0 ? null : i);
}
let n = 0;
for (let i = 0; i < xs.length; i++) {
  if (xs[i] == null) {
    n++;
  }
}
console.log(n);
