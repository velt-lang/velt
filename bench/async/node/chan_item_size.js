// `trySend` + `tryReceive` of 1M items on one task, twice: numbers, then 12-number objects. JS has
// no channels: a minimal array queue with a head index.
class Queue {
  constructor() {
    this.items = [];
    this.head = 0;
  }
  trySend(v) {
    this.items.push(v);
    return true;
  }
  tryReceive() {
    if (this.head == this.items.length) return null;
    const v = this.items[this.head++];
    if (this.head == this.items.length) {
      this.items.length = 0;
      this.head = 0;
    }
    return v;
  }
}

function small(n) {
  const ch = new Queue();
  let sum = 0;
  for (let i = 0; i < n; i++) {
    ch.trySend(i);
    sum += ch.tryReceive() ?? 0;
  }
  return sum;
}

function big(n) {
  const ch = new Queue();
  let sum = 0;
  for (let i = 0; i < n; i++) {
    const x = i;
    ch.trySend({ a: x, b: x, c: x, d: x, e: x, f: x, g: x, h: x, i: x, j: x, k: x, l: x });
    sum += ch.tryReceive()?.l ?? 0;
  }
  return sum;
}

console.log(small(1000000), big(1000000));
