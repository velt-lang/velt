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

async function main() {
  console.log(await retyped());
  console.log(await literal(count));
  console.log(await adapted({ input: count, handler: (n) => "" + n }));
}
main();
