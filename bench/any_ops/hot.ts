// A hot loop of arithmetic and method calls through `any` (#924), for instruction counts.
const xs: any = [] as number[];
for (let i = 0; i < 100000; i++) {
  xs.push(i);
}
let sum: any = 0;
for (let i = 0; i < 100000; i++) {
  const v: any = xs[i];
  sum = sum + v * 2 - 1;
}
const s: any = "hello world";
let c = 0;
for (let i = 0; i < 100000; i++) {
  if (s.includes("wor")) {
    c += s.indexOf("o") as number;
  }
}
console.log(sum, c);
