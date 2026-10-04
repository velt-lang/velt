function total(start, src) {
  let sum = start;
  for (const x of src) {
    sum += x;
    if (sum >= 1000000007) {
      sum -= 1000000007;
    }
  }
  return sum;
}
const xs = [];
for (let i = 0; i < 3000000; i++) {
  xs.push(i);
}
let sum = 0;
for (let round = 0; round < 200; round++) {
  sum = total(sum, xs);
}
console.log(sum);
