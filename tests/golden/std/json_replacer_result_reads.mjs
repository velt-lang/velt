// Node twin of json_replacer_result_reads.velt: prints its .out.

class C {
  a = 1;
  self = null;
}

const c = new C();
c.self = c;

let log = [];

function note(s) {
  log.push(s);
}

function flush(what, text) {
  console.log(what, text, log.join(" "));
  log = [];
}

function attempt(what, f) {
  let text = "";
  try {
    text = f();
  } catch (e) {
    text = e instanceof TypeError ? "TypeError" : e instanceof RangeError ? "RangeError" : "other";
  }
  flush(what, text);
}

class Pair {
  a = 1;
  b = 2;
}

class Leaf {
  toJSON(key) {
    note(`toJSON(${key})`);
    return [key];
  }
}

class Inner {
  toJSON(key) {
    note(`inner(${key})`);
    return [key];
  }
}

class Counted {
  toJSON(key) {
    note(`counted(${key})`);
    return { key: key, inner: new Inner() };
  }
}

class Self {
  n = 3;
  toJSON(key) {
    note(`self(${key})`);
    return this;
  }
}

class Nested {
  toJSON(key) {
    note(`nested(${key})`);
    let s = "";
    try {
      s = JSON.stringify({ q: new Leaf() }, quiet);
    } catch (e) {
      s = "err";
    }
    return s;
  }
}

function quiet(k, v) {
  return v;
}

function dropSelf(k, v) {
  note(`rep(${k})`);
  return k === "self" ? undefined : v;
}

function keepAll(k, v) {
  note(`rep(${k})`);
  return v;
}

function dropGone(k, v) {
  note(`rep(${k})`);
  return k === "gone" ? undefined : v;
}

function replaceGone(k, v) {
  note(`rep(${k})`);
  return k === "gone" ? 0 : v;
}

function getterResult() {
  let reads = 0;
  return {
    a: c,
    get g() {
      reads = reads + 1;
      note(`get g ${reads}`);
      return reads;
    },
  };
}

function reentrant() {
  return {
    a: c,
    get g() {
      note("get g");
      return JSON.stringify({ q: 1 }, quiet);
    },
  };
}

function ordered() {
  return {
    get a() {
      note("get a");
      return 1;
    },
    t: new Leaf(),
    get b() {
      note("get b");
      return 2;
    },
    n: [new Leaf(), 4],
  };
}

function gone() {
  return {
    get gone() {
      note("get gone");
      return 1;
    },
    kept: new Leaf(),
  };
}

let nestedReads = 0;

function nestedGetter() {
  return {
    m: {
      get g() {
        nestedReads = nestedReads + 1;
        return nestedReads;
      },
      t: new Leaf(),
    },
  };
}

class SelfGetter {
  o = {
    get g() {
      nestedReads = nestedReads + 1;
      return nestedReads;
    },
  };
  toJSON(key) {
    note(`selfGetter(${key})`);
    return this;
  }
}

function main() {
  const g = getterResult();
  attempt("getter, cycle dropped:", () => JSON.stringify({ x: 1 }, (k, v) => (k === "" ? g : dropSelf(k, v))));
  const g2 = getterResult();
  attempt("getter, cycle kept:", () => JSON.stringify({ x: 1 }, (k, v) => (k === "" ? g2 : keepAll(k, v))));
  const re = reentrant();
  attempt("reentrant getter:", () => JSON.stringify({ x: 1 }, (k, v) => (k === "" ? re : dropSelf(k, v))));
  attempt("dropped toJSON result:", () =>
    JSON.stringify({ x: 1 }, (k, v) => (k === "" ? { n: new Counted(), gone: new Counted() } : dropGone(k, v))),
  );
  attempt("under a dropped member:", () =>
    JSON.stringify({ x: 1 }, (k, v) => (k === "" ? { gone: { d: new Leaf() }, l: new Leaf() } : dropGone(k, v))),
  );
  attempt("under a replaced member:", () =>
    JSON.stringify({ x: 1 }, (k, v) =>
      k === "" ? { gone: [new Leaf(), new Nested()], l: new Leaf() } : replaceGone(k, v),
    ),
  );
  attempt("after a kept cycle:", () =>
    JSON.stringify({ x: 1 }, (k, v) => (k === "" ? { a: c, t: new Leaf() } : keepAll(k, v))),
  );
  attempt("after a dropped cycle:", () =>
    JSON.stringify({ x: 1 }, (k, v) => (k === "" ? { a: c, t: new Leaf() } : dropSelf(k, v))),
  );
  const o = ordered();
  attempt("order:", () => JSON.stringify({ x: 1 }, (k, v) => (k === "" ? o : keepAll(k, v))));
  const gn = gone();
  attempt("getter dropped:", () => JSON.stringify({ x: 1 }, (k, v) => (k === "" ? gn : dropGone(k, v))));
  attempt("array, cycle dropped:", () =>
    JSON.stringify({ x: 1 }, (k, v) => (k === "" ? [new Leaf(), c, new Self()] : dropSelf(k, v))),
  );
  attempt("array, cycle kept:", () =>
    JSON.stringify({ x: 1 }, (k, v) => (k === "" ? [new Leaf(), c, new Self()] : keepAll(k, v))),
  );
  attempt("nested stringify:", () =>
    JSON.stringify({ x: 1 }, (k, v) => (k === "" ? { a: new Nested(), b: c } : dropSelf(k, v))),
  );
  attempt("nested in a result:", () =>
    JSON.stringify({ x: 1 }, (k, v) =>
      k === "" ? { a: { b: new Leaf() } } : k === "a" ? { c: new Counted(), d: c } : dropSelf(k, v),
    ),
  );
  const ng = nestedGetter();
  attempt("nested getter:", () => {
    nestedReads = 0;
    const s = JSON.stringify({ x: 1 }, (k, v) => (k === "" ? ng : keepAll(k, v)));
    note(`reads ${nestedReads}`);
    return s;
  });
  const sg = new SelfGetter();
  attempt("return this:", () => {
    nestedReads = 0;
    const s = JSON.stringify({ x: 1 }, (k, v) => (k === "" ? { w: { s: sg } } : keepAll(k, v)));
    note(`reads ${nestedReads}`);
    return s;
  });
  const top = new Pair();
  attempt("ancestor returned:", () =>
    JSON.stringify({ x: 1 }, (k, v) => {
      if (k === "") {
        note("rep()");
        return top;
      }
      return k === "b" ? top : keepAll(k, v);
    }),
  );
  attempt("next call:", () => JSON.stringify({ x: [1, new Leaf()] }, keepAll));
}

main();
