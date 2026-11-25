const s = new Set([1, 2, 3]);
const seen = [];
for (const x of s) {
  seen.push(x);
  if (x == 1) {
    s.delete(2);
    s.add(7);
  }
}
console.log(seen.join(","), [...s].join(","));

const grows = new Set(["a"]);
const names = [];
for (const x of grows) {
  names.push(x);
  if (grows.size < 4) {
    grows.add(`${x}${grows.size}`);
  }
}
console.log(names.join(","));

const cleared = new Set([1, 2, 3]);
const firsts = [];
for (const x of cleared) {
  firsts.push(x);
  cleared.clear();
}
console.log(firsts.join(","), cleared.size);

const s2 = new Set([1]);
const it = s2[Symbol.iterator]();
const first = it.next();
s2.add(2);
const second = it.next();
const third = it.next();
s2.add(3);
console.log(first.done, second.done, third.done, it.next().done);

const again = new Set([1, 2]);
const order = [];
for (const x of again) {
  order.push(x);
  if (order.length == 1) {
    again.delete(1);
    again.add(1);
  }
}
console.log(order.join(","));
