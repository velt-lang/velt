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

const v = new Set([1, 2, 3]);
const vs = [];
for (const x of v.values()) {
  vs.push(x);
  if (x == 1) {
    v.delete(2);
    v.add(8);
  }
}
const k = new Set(["a", "b"]);
const ks = [];
for (const x of k.keys()) {
  ks.push(x);
  if (k.size < 4) {
    k.add(`${x}!`);
  }
}
console.log(vs.join(","), ks.join(","));

const e = new Set([1, 2, 3]);
const es = [];
for (const [a, b] of e.entries()) {
  es.push(`${a}:${b}`);
  if (a == 1) {
    e.delete(3);
    e.add(9);
  }
}
console.log(es.join(","));

class Holder {
  items = new Set([5, 6, 7, 8]);
}
const h = new Holder();
const hs = [];
outer: for (const x of h.items.values()) {
  hs.push(x);
  if (x == 6) {
    h.items.add(10);
    continue;
  }
  for (const y of h.items.keys()) {
    if (y == 7) {
      h.items.delete(8);
      continue outer;
    }
  }
}
const firstTwo = [];
for (const x of h.items.values()) {
  firstTwo.push(x);
  h.items.add(x + 100);
  if (firstTwo.length == 2) {
    break;
  }
}
console.log(hs.join(","), firstTwo.join(","), h.items.size);

const cl = new Set([1, 2, 3]);
const cs = [];
for (const x of cl.values()) {
  cs.push(x);
  cl.clear();
}
console.log(cs.join(","), cl.size);
