// `String(v)` of an `any` holding a class instance without `toString`, 200,000 times.
class P {
  x: number;
  constructor(x: number) {
    this.x = x;
  }
}

const vals: any[] = [];
for (let i = 0; i < 100; i++) {
  vals.push(new P(i));
}
let total = 0;
for (let n = 0; n < 200000; n++) {
  total += String(vals[n % 100]).length;
}
console.log(total);
