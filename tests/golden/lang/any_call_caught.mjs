// Node twin of any_call_caught.velt: the same program with its types removed.
// An error thrown by a function called through `any` reaches the caller's `catch`, as in
// JavaScript (#1938, #2353): a closure, a method, a function in a field or an element, one
// boxed through a generic function, one returned by another, from sync and async callers;
// each call after it works as before.
class Box {
  n = 1;
  bad(x) {
    if (x > 1) {
      throw new RangeError("method " + x);
    }
    return x + this.n;
  }
}

class Odd {
  pick(s) {
    if (s === "x") {
      throw "a string";
    }
    return s + s;
  }
}

function plain(f, x) {
  return f(x);
}

function boxed(v) {
  return v;
}

function maker() {
  return () => {
    throw new TypeError("made");
  };
}

async function later(f) {
  await Promise.resolve(0);
  try {
    f(2);
    return "no";
  } catch (e) {
    return "async caught " + `${e}`;
  }
}

async function leaks(f) {
  await Promise.resolve(0);
  return f(2);
}

function main() {
  const f = (x) => {
    if (x === 2) {
      throw new RangeError("x " + x);
    }
    return x * 10;
  };
  try {
    f(2);
  } catch (e) {
    console.log("caught", `${e}`);
  }
  console.log("after", f(3));
  const b = new Box();
  try {
    b.bad(2);
  } catch (e) {
    console.log("caught", `${e}`);
  }
  console.log("method ok", b.bad(1));
  const o = {
    g: (s) => {
      throw new TypeError("field " + s);
    },
  };
  try {
    o.g("a");
  } catch (e) {
    console.log("caught", `${e}`);
  }
  const odd = new Odd();
  try {
    odd.pick("x");
  } catch (e) {
    console.log("caught a value", e);
  }
  console.log(odd.pick("y"));
  const fs = [f, (n) => n + 1];
  try {
    fs[0](2);
  } catch (e) {
    console.log("element", `${e}`);
  }
  console.log("element ok", fs[1](1));
  // A computed member and an optional call: velt/lang/this_fn calls them for the program.
  try {
    fs[0](2);
  } catch (e) {
    console.log("element again", `${e}`);
  }
  const k = "g";
  try {
    o[k]("b");
  } catch (e) {
    console.log("computed", `${e}`);
  }
  try {
    o.g?.("c");
  } catch (e) {
    console.log("optional", `${e}`);
  }
  const g = boxed((n) => {
    if (n < 0) {
      throw new Error("negative");
    }
    return n;
  });
  try {
    g(-1);
  } catch (e) {
    console.log("generic", `${e}`);
  }
  const m = maker;
  try {
    m()();
  } catch (e) {
    console.log("returned", `${e}`);
  }
  try {
    plain(f, 2);
  } catch (e) {
    console.log("through fn", `${e}`);
  }
  try {
    try {
      f(2);
    } catch (e) {
      console.log("inner", `${e}`);
      throw e;
    }
  } catch (e2) {
    console.log("rethrown", `${e2}`);
  }
  try {
    try {
      f(2);
    } catch (e) {
      f(2);
    }
  } catch (e3) {
    console.log("throw in catch", `${e3}`);
  }
  try {
    f(2);
  } catch (e) {
    console.log("with finally", `${e}`);
  } finally {
    console.log("finally ran");
  }
  later(f).then((s) => console.log(s));
  leaks(f).catch((e) => console.log("rejected", `${e}`));
}

main();
