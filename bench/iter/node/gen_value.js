function* range(n) {
  let i = 0;
  while (i < n) {
    yield i;
    i++;
  }
}
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
let sum = 0;
for (let round = 0; round < 20; round++) {
  sum = total(sum, range(30000000));
}
console.log(sum);
