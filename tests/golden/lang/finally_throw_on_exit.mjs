// Node twin of finally_throw_on_exit.velt: the same program with its types removed.
// An error thrown in a `finally` block that runs on a `return`, `break` or `continue` out of its
// `try` (or out of the `catch`) leaves the whole `try` statement, to a `catch` outside it, as
// in Node: the `try`'s own `catch` is not entered. A `throw`, a call through `any` that throws,
// and calling a value that is not a function.
class Res {
  name;
  constructor(name) {
    this.name = name;
  }
}

function fromBody(n) {
  const r = new Res("body");
  try {
    if (n > 0) {
      return 1;
    }
    throw new RangeError("body");
  } catch (e) {
    console.log("inner", `${e}`, r.name);
    return 2;
  } finally {
    if (n > 0) {
      throw new TypeError("fin");
    }
  }
}

function fromCatch() {
  try {
    throw new RangeError("body");
  } catch (e) {
    const r = new Res("catch");
    console.log("in catch", `${e}`, r.name);
    return 2;
  } finally {
    throw new TypeError("fin after catch");
  }
}

function loops() {
  const out = [];
  for (let i = 0; i < 3; i++) {
    try {
      try {
        if (i === 0) {
          continue;
        }
        if (i === 1) {
          break;
        }
      } catch (e) {
        out.push("wrong " + `${e}`);
      } finally {
        throw new Error("fin " + i);
      }
    } catch (e) {
      out.push(`${e}`);
    }
  }
  return out.join(",");
}

const boom = () => {
  throw new RangeError("through any");
};
const notFn = 5;

function viaAny(f) {
  try {
    return 1;
  } catch (e) {
    console.log("inner", `${e}`);
    return 2;
  } finally {
    f();
  }
}

function outer(e) {
  console.log("outer", e instanceof TypeError, `${e}`);
}
try {
  console.log("result", fromBody(1));
} catch (e) {
  outer(e);
}
try {
  console.log("result", fromBody(0));
} catch (e) {
  outer(e);
}
try {
  console.log("result", fromCatch());
} catch (e) {
  outer(e);
}
console.log("result", loops());
try {
  console.log("result", viaAny(boom));
} catch (e) {
  outer(e);
}
// Calling a value that is not a function in the `finally`: a `catch` around the whole `try`
// statement, in the same function, takes the `TypeError`.
function nested(f) {
  try {
    try {
      return 1;
    } finally {
      f();
    }
  } catch (e) {
    console.log("same function", e instanceof TypeError, `${e}`);
    return 3;
  }
}
console.log("result", nested(notFn));
