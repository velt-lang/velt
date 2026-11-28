// The Node twin of tests/golden/std/promise_error_set_variable_rejects.velt; its .out is what Node 22 prints.

// What a promise rejects with when that comes through a function value or a store of a
// `Promise<T>` written without an error type (#3893): the check with error-set variables assumes
// their sets as known when it meets them, and when a set turns out to hold an error, the program
// is checked again with it. Each rejection reaches its `catch`. The output is Node 22's.

class Negative extends Error {}

class Refused extends Error {}

function later(n        )                  {
  return new Promise((r, reject) => {
    if (n < 0) {
      reject(new Negative("negative"));
      return;
    }
    r(n);
  });
}

class Gate {
  open                        = (why        ) => {};
}

async function main()                {
  // A store of the rejecting promise as a `Promise<number> | null`.
  const maybe                         = later(-1);
  try {
    if (maybe != null) {
      console.log(await maybe);
    }
  } catch (e) {
    console.log("stored: caught", e instanceof Negative);
  }
  // A `then` callback calling a function value that throws.
  const g = new Gate();
  g.open = (why        ) => {
    if (why !== "") {
      throw new Refused("refused");
    }
  };
  try {
    await Promise.resolve().then(() => g.open("then"));
  } catch (e) {
    console.log("then: caught", e instanceof Refused);
  }
  // The same function value called by a `finally` callback.
  try {
    await Promise.resolve(2).finally(() => g.open("finally"));
  } catch (e) {
    console.log("finally: caught", e instanceof Refused);
  }
  // And with a value it doesn't throw for: the promise fulfills.
  const n = await Promise.resolve(3).then((n) => {
    g.open("");
    return n;
  });
  console.log("then:", n);
}

main();
