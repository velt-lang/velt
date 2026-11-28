// Node twin of json_replacer_results.velt: prints its .out.

class Money {
  cents = 5;
  toJSON(key) {
    return `${key}:${this.cents}`;
  }
}

class Stamp {
  toJSON() {
    return new Date(0);
  }
}

class Loose {
  toJSON() {
    return new Date(0);
  }
}

function cyclic(k, v) {
  if (k === "a") {
    const o = [1];
    o.push(o);
    return o;
  }
  return v;
}

function deep(k, v) {
  if (k === "b") {
    const inner = ["x"];
    const o = [inner];
    inner.push(o);
    return { list: o };
  }
  return v;
}

function objects(k, v) {
  if (k === "d") return new Date(0);
  if (k === "m") return new Map();
  if (k === "c") return new Money();
  return v;
}

function keep(k, v) {
  return v;
}

function attempt(what, f) {
  try {
    console.log(what, f());
  } catch (e) {
    console.log(what, e instanceof TypeError ? "TypeError" : "other");
  }
}

attempt("cyclic result:", () => JSON.stringify({ a: 1, b: 2 }, cyclic));
attempt("cycle inside:", () => JSON.stringify({ a: 1, b: 2 }, deep));
attempt("after:", () => JSON.stringify({ b: [2], c: "x" }, cyclic));
attempt("objects:", () => JSON.stringify({ d: 1, m: 2, c: 3 }, objects));
attempt("toJSON's Date:", () => JSON.stringify({ s: new Stamp(), l: new Loose() }));
attempt("with a replacer:", () => JSON.stringify({ s: new Stamp(), l: new Loose() }, keep));
attempt("in an array:", () => JSON.stringify([new Stamp(), new Loose()], keep));
const l = new Loose();
attempt("through any:", () => JSON.stringify(l));
