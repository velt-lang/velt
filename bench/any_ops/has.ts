// `key in x` through `any` (#823) in a hot loop, for instruction counts
// (`bench/any_ops/count.sh <velt> <std> <rt> bench/any_ops/has.ts`): an object's field and a
// missing key, a class instance's method and an array's `length`.
class Point {
  x = 1;
  norm(): number {
    return this.x;
  }
}
const o: any = { a: 1, b: "s" };
const p: any = new Point();
const xs: any = [1, 2, 3];
let n = 0;
for (let i = 0; i < 100000; i++) {
  if ("b" in o) n++;
  if ("zz" in o) n++;
  if ("norm" in p) n++;
  if ("length" in xs) n++;
}
console.log(n);
