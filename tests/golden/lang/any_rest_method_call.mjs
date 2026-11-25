// Node twin of any_rest_method_call.velt: the same program with its types removed.
// A method with a rest parameter called through `any`: the arguments from its position on are
// the rest array, as at a typed call (#3812).
// No argument written is not `undefined` written.
class Counter {
  join(...p) {
    return p.length;
  }
}

class C {
  tag = "c";
  join(...p) {
    return this.tag + ":" + p.join("+");
  }
  sum(first, ...more) {
    let s = first;
    for (const m of more) {
      s += m;
    }
    return s;
  }
  count(label, flag, ...rest) {
    return label + flag + rest.length;
  }
  async later(...p) {
    return p.length;
  }
}

class D extends C {
  join(...p) {
    return "d(" + super.join(...p) + ")";
  }
}

class Box {
  items = [];
  add(...xs) {
    for (const x of xs) {
      this.items.push(x);
    }
    return this.items.length;
  }
}

async function main() {
  const c = new C();
  console.log(c.join("x", "y"), c.join(), c.join("one"));
  console.log(c.sum(1), c.sum(1, 2), c.sum(1, 2, 3, 4));
  console.log(c.count("l", true), c.count("l", false, 1, "two", null));
  console.log(await c.later(), await c.later(1, 2, 3));
  const d = new D();
  console.log(d.join("a", "b"), d.join(), d.sum(5, 5));
  const b = new Box ();
  console.log(b.add(), b.add(1), b.add(2, 3), b.items);
  const j = new C();
  const aj = j;
  console.log(aj.join("i", "j"), aj.join());
  // An object literal's function value with a rest parameter, as before.
  const obj = { plain: (...xs) => xs.join("-") };
  const lit = obj;
  console.log(lit.plain("p", "q"), lit.plain());
  const l = new Counter();
  console.log(l.join(), l.join(undefined), l.join(undefined, undefined), l.join(null), l.join("a"));
  const arr = [1, 2];
  console.log(arr.join(), arr.join(undefined), arr.join("-"));
  // Through a proxy.
  const p = new Proxy(new C(), {});
  console.log(p.join("x", "y"), p.join(), p.sum(4, 5));
}

main();
