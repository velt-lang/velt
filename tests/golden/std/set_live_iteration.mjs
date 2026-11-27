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
// A `for...of` over the set itself, in the same shapes: a field, a label, `break`, a call
// returning the set, and a `let` reassigned in the body (the loop keeps walking the first set).
class Box {
  items = new Set([5, 6, 7, 8]);
}
const bx = new Box();
const bs = [];
round: for (const x of bx.items) {
  bs.push(x);
  if (x == 6) {
    bx.items.add(10);
    continue;
  }
  for (const y of bx.items) {
    if (y == 7) {
      bx.items.delete(8);
      continue round;
    }
  }
}
const bf = [];
for (const x of bx.items) {
  bf.push(x);
  bx.items.add(x + 100);
  if (bf.length == 2) {
    break;
  }
}
console.log(bs.join(","), bf.join(","), bx.items.size);

function pick() {
  return bx.items;
}
let total = 0;
for (const x of pick()) {
  total += x;
  if (x == 5) {
    bx.items.delete(6);
  }
}
let cur = new Set(["p", "q"]);
const cv = [];
for (const x of cur) {
  cv.push(x);
  cur = new Set(["z"]);
}
console.log(total, cv.join(","), [...cur].join(","));

// Over a parameter: one the body doesn't assign is walked live; one the body or a closure
// assigns goes on with the set the loop started with.
function liveParam(s) {
  const out = [];
  for (const x of s) {
    out.push(x);
    if (x == 1) {
      s.delete(2);
      s.add(4);
    }
  }
  return out.join(",");
}
function swapParam(s) {
  const out = [];
  for (const x of s) {
    out.push(x);
    s = new Set([50]);
  }
  return out.join(",") + "/" + [...s].join(",");
}
function closureParam(s) {
  const out = [];
  const swap = () => {
    s = new Set([60]);
  };
  for (const x of s) {
    out.push(x);
    swap();
  }
  return out.join(",") + "/" + [...s].join(",");
}
console.log(
  liveParam(new Set([1, 2, 3])),
  swapParam(new Set([7, 8])),
  closureParam(new Set([5, 6])),
);
