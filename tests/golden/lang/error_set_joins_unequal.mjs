// The Node twin of tests/golden/lang/error_set_joins_unequal.velt; its .out is what Node 22 prints.

// Two error types with error-set variables that a value passes between both ways, of the same
// written members (a variable holding a function calling two function values, assigned another
// such function), are one type only when the solved sets make them equal (`VarSets::joins`,
// #3925). Here the solved sets differ: the assigned function's calls throw, so each rejection
// must still reach its `catch`, through an `await`, a `then` and a `catch` handler. The output
// is Node 22's.

class A extends Error {}

class B extends Error {}

// A variable holding an async arrow, assigned one whose calls throw, called and awaited.
async function awaited(a, b, c, d) {
  let f = async () => {
    a();
    b();
  };
  const g = async () => {
    c();
    d();
  };
  try {
    await f();
    console.log("f ok");
  } catch (e) {
    console.log("f", e instanceof A);
  }
  f = g;
  try {
    await f();
    console.log("g ok");
  } catch (e) {
    console.log("g", e instanceof A);
  }
}

// The same, the variable passed as a `then` and a `catch` handler.
async function handlers(a, b, c, d) {
  let h = async () => {
    a();
    b();
  };
  const k = async () => {
    c();
    d();
  };
  h = k;
  try {
    await Promise.resolve(1).then(h);
    console.log("then ok");
  } catch (e) {
    console.log("then", e instanceof B);
  }
  try {
    await Promise.reject(new A("x")).catch(h);
    console.log("catch ok");
  } catch (e) {
    console.log("catch", e instanceof B);
  }
}

// A variable holding a plain arrow, as a `catch` handler before and after it is assigned one
// whose calls throw, and as both handlers of a `then`.
async function pair(a, b, c, d) {
  let h = () => {
    a();
    b();
  };
  const k = () => {
    c();
    d();
  };
  try {
    await Promise.reject(new A("first")).catch(h);
    console.log("h ok");
  } catch (e) {
    console.log("h", e instanceof B);
  }
  h = k;
  try {
    await Promise.reject(new A("second")).catch(h);
    console.log("k ok");
  } catch (e) {
    console.log("k", e instanceof B);
  }
  try {
    await Promise.resolve(1).then(h, h);
    console.log("then pair ok");
  } catch (e) {
    console.log("then pair", e instanceof B);
  }
}

async function main() {
  await awaited(
    () => {},
    () => {},
    () => {
      throw new A("c");
    },
    () => {},
  );
  await handlers(
    () => {},
    () => {},
    () => {
      throw new B("c");
    },
    () => {},
  );
  await pair(
    () => {},
    () => {},
    () => {
      throw new B("c");
    },
    () => {},
  );
}

main();
