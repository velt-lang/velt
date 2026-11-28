// Node twin of any_call_caught_union.velt: the same program with its types removed.
// converted to `any`: a call through the `any` passes its error to the caller's `catch` (#1938).

function pick(n) {
  if (n > 0) {
    return () => {
      throw new RangeError("union " + n);
    };
  }
  return "text";
}

const h = pick(1);
try {
  h();
} catch (e) {
  console.log("caught", `${e}`);
}
console.log(typeof pick(0));

const box = {
  run: (n) => {
    if (n > 1) {
      throw new TypeError("field " + n);
    }
    return n;
  },
};
try {
  console.log(box.run(1));
  box.run(2);
} catch (e) {
  console.log("caught", `${e}`);
}
