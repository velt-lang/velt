// Node twin of json_replacer_module_state.velt: prints its .out. Node's JSON.stringify returns
// undefined where Velt's throws a TypeError (its result is a string); the twin throws it too.

const seen = [];

function logger(k, v) {
  seen.push(k);
  return v;
}

function twice(k, v) {
  if (k === "x") {
    const a = [];
    a.push(v);
    a.push(v);
    return a;
  }
  return v;
}

function gone(k, v) {
  seen.push(`gone ${k}`);
  return undefined;
}

console.log(JSON.stringify({ x: 1, y: [2, 3] }, logger));
console.log(JSON.stringify({ x: { q: 1 }, y: 2 }, twice));
console.log(seen.join(","));
const out = JSON.stringify({ x: 1 }, gone);
if (out === undefined) {
  throw new TypeError("JSON.stringify: the replacer returned undefined for the value itself");
}
console.log(out);
