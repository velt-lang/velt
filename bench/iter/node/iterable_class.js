class RangeIter {
  i = 0;
  constructor(n) {
    this.n = n;
  }
  next() {
    if (this.i < this.n) {
      const v = this.i;
      this.i++;
      return { done: false, value: v };
    }
    return { done: true };
  }
}
class Range {
  constructor(n) {
    this.n = n;
  }
  [Symbol.iterator]() {
    return new RangeIter(this.n);
  }
}
let sum = 0;
for (let round = 0; round < 20; round++) {
  for (const x of new Range(30000000)) {
    sum += x;
    if (sum >= 1000000007) {
      sum -= 1000000007;
    }
  }
}
console.log(sum);
