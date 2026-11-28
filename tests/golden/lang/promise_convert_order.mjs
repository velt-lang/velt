// Node twin of promise_convert_order.velt: prints its .out.
async function fail(x) {
  throw new Error(x);
}

async function retyped() {
  let out = "retyped:";
  const kept = fail("kept");
  try {
    await kept;
  } catch (e) {}
  const before = fail("before");
  try {
    await Promise.all([before, kept]);
  } catch (e) {
    out += " " + String(e);
  }
  return out;
}

async function literal(s) {
  let out = "literal:";
  const kept = s.validate("kept");
  try {
    await kept;
  } catch (e) {}
  try {
    await Promise.all([s.validate("before"), kept]);
  } catch (e) {
    out += " " + String(e);
  }
  return out;
}

async function adapted(options) {
  let out = "adapted:";
  const kept = options.input.validate("kept");
  try {
    await kept;
  } catch (e) {}
  const before = options.input.validate("before");
  try {
    await Promise.all([before, kept]);
  } catch (e) {
    out += " " + String(e);
  }
  const fresh = options.input.validate("fresh");
  const other = options.input.validate("other");
  try {
    await Promise.all([fresh, other]);
  } catch (e) {
    out += " / " + String(e);
  }
  return out;
}

const count = {
  async validate(value) {
    if (typeof value === "number") {
      return { value: value + 1 };
    }
    throw new Error(String(value));
  },
};

const log = [];

async function later(v, tag) {
  log.push(tag + "1");
  await null;
  log.push(tag + "2");
  await null;
  log.push(tag + "3");
  return v;
}

async function ticks(tag) {
  for (const s of ["a", "b", "c", "d"]) {
    await null;
    log.push(tag + s);
  }
}

async function callbacks() {
  const p = later(1, "C");
  const c = p;
  const t = ticks("x");
  const a = c.then(() => log.push("c1"));
  const b = p.then(() => log.push("p1"));
  await Promise.all([a, b, t]);
  return "callbacks: " + log.join(" ");
}

async function raced() {
  const kept = later(1, "K");
  await kept;
  const fresh = Promise.resolve(2);
  return "raced: " + String(await Promise.race([fresh, kept]));
}

async function adaptedRace(options) {
  const kept = options.input.validate(1);
  await kept;
  const fresh = options.input.validate(5);
  const r = await Promise.race([fresh, kept]);
  return "adapted race: " + String(r.value);
}

async function main() {
  console.log(await retyped());
  console.log(await literal(count));
  console.log(await adapted({ input: count, handler: (n) => "" + n }));
  console.log(await callbacks());
  console.log(await raced());
  console.log(await adaptedRace({ input: count, handler: (n) => "" + n }));
}
main();
