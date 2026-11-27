// Node twin of proxy_array_trap_throws.velt: the same program with its types removed.
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

// A proxy of a proxy: the inner trap's error leaves the outer method.
const inner = guarded("set", "3");
const outer = new Proxy(inner.p, {});
attempt("nested", () => outer.push(9));
attempt("nested again", () => outer.length);


// Rethrown from a `catch`, and from an async caller.
try {
  try {
    guarded("deleteProperty", "2").p.pop();
  } catch (e) {
    log.push(`rethrowing ${e}`);
    throw e;
  }
} catch (e) {
  log.push(`rethrown ${e instanceof TypeError} ${e}`);
}
async function later() {
  await Promise.resolve(0);
  try {
    guarded("set", "3").p.push(4);
    return "no";
  } catch (e) {
    return "async " + `${e}`;
  }
}

for (const line of log) {
  console.log(line);
}
console.log(await later());
