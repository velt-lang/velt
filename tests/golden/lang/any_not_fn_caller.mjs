// Node twin of any_not_fn_caller.velt: the same program with its types removed.
function callMissing(x) {
  return x.missing();
}

function twice(x) {
  return callMissing(x) * 2;
}

class Good {
  side = 3;
  area() {
    return o.square(this.side);
  }
}

class Bad {
  area() {
    return o.a();
  }
}

function total(shapes) {
  let t = 0;
  for (const s of shapes) {
    t += s.area();
  }
  return t;
}

const o = { a: 1, square: (x) => x * x };
console.log("before");
try {
  callMissing(o);
} catch (e) {
  console.log("caught", e.message);
}
try {
  console.log(twice(o));
} catch (e) {
  console.log("caught twice", e.name, e.message);
}
console.log(total([new Good()]));
try {
  total([new Good(), new Bad()]);
} catch (e) {
  console.log("caught area", e.message);
}
try {
  [1, 2].forEach((x) => o.a(x));
} catch (e) {
  console.log("caught forEach", e.message);
}
console.log("after");
