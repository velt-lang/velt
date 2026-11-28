// Node twin of any_not_fn_async.velt: the same program with its types removed.
const o = { ob: 5, f: (x) => x + 1 };
const k = "ob";

async function decl() {
  o.ob();
}

class C {
  async m() {
    o[k]();
    return 1;
  }
}

async function* agen() {
  yield 1;
  o.ob();
}

function* gen() {
  yield 1;
  o.ob();
}

async function inArgs() {
  await Promise.resolve(o.ob());
}

async function nested() {
  await (async () => {
    await (async () => {
      o.ob();
    })();
  })();
}

async function optional() {
  o?.ob();
}

async function inFinally() {
  try {
    console.log("try body");
  } finally {
    o.ob();
  }
}

async function inCatch() {
  try {
    throw new Error("first");
  } catch (e) {
    o.ob();
  }
}

async function afterAwait() {
  const v = await Promise.resolve(2);
  console.log("awaited", v);
  return o.ob(v);
}

function sync() {
  return o.ob();
}

function outer() {
  return sync() + 1;
}

async function report(what, p) {
  try {
    await p;
    console.log(what, "resolved");
  } catch (e) {
    console.log(what, e.name, e.message.endsWith("is not a function"));
  }
}

async function main() {
  const p = (async () => {
    o.ob();
  })();
  try {
    await p;
  } catch (e) {
    console.log("arrow", e.message);
  }
  await report("decl", decl());
  await report("method", new C().m());
  const g = agen();
  console.log((await g.next()).value);
  await report("async gen", g.next());
  const s = gen();
  console.log(s.next().value);
  try {
    s.next();
  } catch (e) {
    console.log("gen", e.message);
  }
  await report("await operand", inArgs());
  await report("nested", nested());
  await report("optional", optional());
  await report("finally", inFinally());
  await report("catch body", inCatch());
  await report("after await", afterAwait());
  await report("then", Promise.resolve(1).then((v) => o.ob(v)));
  try {
    outer();
  } catch (e) {
    console.log("sync caller", e.message);
  }
  console.log(o.f(1), await (async () => o.f(2))());
}

main();
