// Node twin of object_copy_optional_unknown.velt: the same program with its types removed.
function take(o) {
  return String(o.input) + ":" + String(o.args) + ":" + ("input" in o) + ":" + ("args" in o);
}

function any(o) {
  return typeof o.input + ":" + ("input" in o);
}

const x = { input: "a" };
console.log(take(x));
const y = {};
console.log(take(y));
const z = { input: undefined };
console.log(take(z));
const w = { input: 2 };
console.log(any(w));
console.log(any(y));
