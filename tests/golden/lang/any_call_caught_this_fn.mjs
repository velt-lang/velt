// Node twin of any_call_caught_this_fn.velt: the same program with its types removed.
function tagged(x) {
  if (x === 2) {
    throw new RangeError("tagged " + x);
  }
  return x + (this === null ? 1 : 2);
}
const t = tagged;
const one = [1];
const two = [2];
console.log("apply", t.apply(null, one));
try {
  t.apply(null, two);
} catch (e) {
  console.log("apply caught", `${e}`);
}
const f = (x) => {
  if (x === 2) {
    throw new RangeError("x " + x);
  }
  return x;
};
const fs = [f];
try {
  fs[0](2);
} catch (e) {
  console.log("element", `${e}`);
}
