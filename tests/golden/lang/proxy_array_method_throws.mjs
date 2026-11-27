// Node twin of proxy_array_method_throws.velt: the same program with its types removed.
const log = [];

function guarded(trap, key) {
  const raw = [1, 2, 3];
  const handler = {};
  handler[trap] = (t, k, ...rest) => {
    if (String(k) === key) {
      throw new TypeError(`${trap} ${String(k)}`);
    }
    return Reflect[trap](t, k, ...rest);
  };
  return { raw, p: new Proxy(raw, handler) };
}

function attempt(label, run) {
  try {
    const r = run();
    log.push(`${label}: ok ${r}`);
  } catch (e) {
    log.push(`${label}: caught ${e}`);
  }
}

// A trap that throws while an array method runs; the same proxy works after it.
const s = guarded("set", "3");
attempt("push set", () => s.p.push(4));
attempt("push again", () => s.p.push());
log.push(`raw ${s.raw.join()}`);

const d = guarded("deleteProperty", "2");
attempt("pop delete", () => d.p.pop());
attempt("pop again", () => d.p.length);
log.push(`raw ${d.raw.join()}`);

const g = guarded("get", "0");
attempt("shift get", () => g.p.shift());
attempt("join after", () => g.p.slice(1).join("-"));

const h = guarded("has", "1");
attempt("splice has", () => h.p.splice(0, 1));
attempt("includes after", () => h.p.includes(3));
log.push(`raw ${h.raw.join()}`);

// The methods left by an error left no state behind: the raw arrays and the proxies work.
attempt("raw arrays", () => {
  s.raw.push(4);
  d.raw.unshift(0);
  g.raw.reverse();
  h.raw.splice(1, 1);
  return [s.raw.join(), d.raw.join(), g.raw.join(), h.raw.join()].join(" | ");
});
attempt("proxies again", () =>
  [s.p.length, d.p.indexOf(0), g.p.slice(1).join(), h.p.length, Reflect.has(d.p, "3")].join(" "),
);

// A callback that throws: forEach, map, filter, find, some, sort.
const q = new Proxy([1, 2, 3], {});
function bad(x) {
  if (x === 2) {
    throw new RangeError("cb " + x);
  }
  return x * 10;
}
attempt("forEach", () => q.forEach((x) => bad(x)));
attempt("map", () => q.map((x) => bad(x)).join());
attempt("filter", () => q.filter((x) => bad(x) > 0).length);
attempt("find", () => q.find((x) => bad(x) > 100));
attempt("some", () => q.some((x) => bad(x) > 100));
attempt("map after", () => q.map((x) => x + 1).join());
const order = new Proxy([3, 1, 2], {});
attempt("sort", () =>
  order.sort((a, b) => {
    if (a === 2 || b === 2) {
      throw new Error("cmp");
    }
    return a - b;
  }),
);
attempt("sort after", () => order.sort((a, b) => a - b).join());

const bigRaw = [];
for (let i = 0; i < 40; i++) {
  bigRaw.push(40 - i);
}
const big = new Proxy(bigRaw, {});
attempt("sort big", () =>
  big.sort((a, b) => {
    if (a === 3 || b === 3) {
      throw new Error("late");
    }
    return a - b;
  }),
);
attempt("sort big after", () => big.sort((a, b) => a - b).slice(0, 3).join());

// A value that isn't an `Error` keeps its type.
attempt("string", () =>
  q.forEach((x) => {
    if (x === 3) {
      throw "plain " + x;
    }
  }),
);

// A proxy of a proxy: the inner trap's error leaves the outer method.
const inner = guarded("set", "3");
const outer = new Proxy(inner.p, {});
attempt("nested", () => outer.push(9));
attempt("nested again", () => outer.length);

// Thrown inside a `catch`, rethrown, with `finally`.
try {
  try {
    guarded("set", "3").p.push(4);
  } catch (e) {
    log.push(`inner ${e}`);
    throw new Error("from catch");
  }
} catch (e) {
  log.push(`outer ${e}`);
}
try {
  try {
    q.forEach((x) => bad(x));
  } catch (e) {
    log.push(`rethrowing ${e}`);
    throw e;
  } finally {
    log.push("finally");
  }
} catch (e) {
  log.push(`rethrown ${e instanceof RangeError} ${e}`);
}

// From an async caller, and an error nothing catches in a function: the caller's catch.
async function later() {
  await Promise.resolve(0);
  try {
    q.map((x) => bad(x));
    return "no";
  } catch (e) {
    return "async " + e;
  }
}
function leaks() {
  return guarded("set", "3").p.push(4);
}
attempt("through a function", () => leaks());

for (const line of log) {
  console.log(line);
}
console.log(await later());
