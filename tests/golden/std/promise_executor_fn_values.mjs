// The Node twin of tests/golden/std/promise_executor_fn_values.velt; its .out is what Node 22 prints.

class Negative extends Error {}
class Refused extends Error {}

let open                      = (n        ) => {};

function later(n        )                  {
  return new Promise((r, reject) => {
    if (n < 0) {
      reject(new Negative("negative"));
      return;
    }
    r(n);
  });
}

function laterGeneric   (v   , bad         )             {
  return new Promise((r, reject) => {
    if (bad) {
      reject(new Negative("negative"));
      return;
    }
    r(v);
  });
}

let saved                      = (n        ) => {};

function stored()                  {
  return new Promise((r, reject) => {
    saved = r;
    if (saved === open) {
      reject(new Negative("never"));
    }
  });
}

function nested(n        )                  {
  return new Promise((r, reject) => {
    queueMicrotask(() => {
      if (n < 0) {
        reject(new Negative("negative"));
        return;
      }
      r(n);
    });
  });
}

function resolvers(n        )                  {
  const w = Promise.withResolvers                  ();
  if (n < 0) {
    w.reject(new Negative("negative"));
  } else {
    w.resolve(n);
  }
  return w.promise;
}

async function relay(n        )                  {
  const w = Promise.withResolvers        ();
  const v = await new Promise        ((r, reject) => {
    if (n < 0) {
      reject(new Negative("negative"));
      return;
    }
    const k = r;
    w.resolve(n * 10);
    k(n);
  });
  return v + (await w.promise);
}

function apply   (f                , x   )       {
  f(x);
}

async function main()                {
  open = (n        ) => {
    throw new Refused("refused");
  };
  console.log(await later(1));
  console.log(await laterGeneric        (2, false));
  console.log(await nested(3));
  console.log(await resolvers(4));
  console.log(await relay(5));
  try {
    await later(-1);
  } catch (e) {
    console.log("negative", e instanceof Negative);
  }
  try {
    await Promise.resolve().then(() => open(1));
  } catch (e) {
    console.log("then", e instanceof Refused);
  }
  try {
    await Promise.resolve().finally(() => open(1));
  } catch (e) {
    console.log("finally", e instanceof Refused);
  }
  try {
    await Promise.reject(new Error("x")).catch(() => open(1));
  } catch (e) {
    console.log("catch", e instanceof Refused);
  }
  try {
    await Promise.resolve().then(async () => open(1));
  } catch (e) {
    console.log("async", e instanceof Refused);
  }
  try {
    await Promise.resolve(5).then((x) => apply(open, x));
  } catch (e) {
    console.log("generic", e instanceof Refused);
  }
  const p = stored();
  saved(6);
  console.log(await p);
  open = (n        ) => {
    console.log("open", n);
  };
  await Promise.resolve().then(() => open(7));
  open = (n        ) => {
    throw new Refused("again");
  };
  try {
    await Promise.resolve().then(() => open(8));
  } catch (e) {
    console.log("reassigned", e instanceof Refused);
  }
}

main();
