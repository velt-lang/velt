// Node twin of json_replacer_nested_calls.velt: prints its .out.

class C {
  a = 1;
  self = null;
}

const c = new C();
c.self = c;

function attempt(what, f) {
  try {
    console.log(what, f());
  } catch (e) {
    const kind = e instanceof TypeError ? "TypeError" : e instanceof RangeError ? "RangeError" : "other";
    console.log(what, kind);
  }
}

function inner(k, v) {
  return v;
}

class T {
  n = 5;
  toJSON(key) {
    let s = "";
    try {
      s = JSON.stringify({ q: 1 }, inner);
    } catch (e) {
      s = "err";
    }
    return { inner: s };
  }
}

class Failing {
  toJSON(key) {
    let s = "";
    try {
      s = JSON.stringify({ q: c }, inner);
    } catch (e) {
      s = e instanceof TypeError ? "TypeError" : "other";
    }
    return { inner: s };
  }
}

function keep(k, v) {
  return k === "" ? { a: c, t: new T() } : v;
}

function drop(k, v) {
  if (k === "self") {
    return undefined;
  }
  return k === "" ? { a: c, t: new T() } : v;
}

function dropFailing(k, v) {
  if (k === "self") {
    return undefined;
  }
  return k === "" ? { a: c, f: new Failing() } : v;
}

function keepFailing(k, v) {
  return k === "" ? { a: c, f: new Failing() } : v;
}

function endless(k, v) {
  const o = new C();
  o.self = o;
  if (k === "self") {
    return { a: 2, next: o, t: new T() };
  }
  return v;
}

function endlessCalling(k, v) {
  const o = new C();
  o.self = o;
  if (k === "self") {
    let s = "";
    try {
      s = JSON.stringify({ q: 1 }, inner);
    } catch (e) {
      s = "err";
    }
    return { a: 2, next: o, s: s };
  }
  return v;
}

attempt("kept:", () => JSON.stringify({ x: 1 }, keep));
attempt("dropped:", () => JSON.stringify({ x: 1 }, drop));
attempt("inner throws, dropped:", () => JSON.stringify({ x: 1 }, dropFailing));
attempt("inner throws, kept:", () => JSON.stringify({ x: 1 }, keepFailing));
attempt("after:", () => JSON.stringify({ x: 1 }, drop));
attempt("endless:", () => JSON.stringify(c, endless));
attempt("endless calling:", () => JSON.stringify(c, endlessCalling));
attempt("after endless:", () => JSON.stringify({ x: 1 }, drop));
attempt("plain after:", () => JSON.stringify({ y: [1, 2] }));
