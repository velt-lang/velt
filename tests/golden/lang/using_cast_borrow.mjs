// Node twin of using_cast_borrow.velt (#3451): prints using_cast_borrow.out. Node 22 runs
// `using` declarations only behind a flag that does not dispose yet, so each is written as
// tsc lowers it: the body in a `try`, the dispose in its `finally`.
class Base {
  constructor(name) {
    this.name = name;
  }
  [Symbol.dispose]() {
    console.log("dispose " + this.name);
  }
}

class Sub extends Base {
  hello() {
    return "hello " + this.name;
  }
}

function using(m, body) {
  try {
    return body(m);
  } finally {
    m[Symbol.dispose]();
  }
}

function called() {
  return using(new Sub("a"), (m) => {
    const s = m.hello();
    console.log("in called");
    return s;
  });
}

function tested() {
  return using(new Sub("b"), (m) => {
    const yes = m instanceof Sub && m.hello() === "hello b";
    console.log("in tested");
    return yes;
  });
}

function named() {
  return using(new Sub("d"), (m) => {
    const s = m;
    const text = s.hello();
    console.log("in named");
    return text;
  });
}

function parts() {
  return using(new Base("c"), (m) => {
    const [{ name }] = [m];
    const [, k] = [m, 2];
    console.log("in parts", k);
    return name;
  });
}

console.log(called());
console.log(tested());
console.log(parts());
console.log(named());
