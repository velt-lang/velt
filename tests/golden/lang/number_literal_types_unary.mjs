// Node twin of number_literal_types_unary.velt: the same program without its types.
function half(x) {
  return x / 2;
}

function kind(x) {
  return typeof x;
}

function negate(n) {
  return -n;
}

class Flag {
  bit = 1;
  flip() {
    return ~this.bit;
  }
}

function run() {
  const bits = [0, 1];
  for (const n of bits) {
    console.log(-n, +n, Object.is(-n, -0), -n / 2);
    console.log(~n, ~n / 2);
    const neg = -n;
    const pos = +n;
    const not = ~n;
    console.log("typed", neg + 0.5, pos + 0.5, not + 0.5);
    console.log("nested", -(-n), ~~n, -~n, ~-n, +-n, -(+n) / 4);
    console.log("call", half(-n), half(+n), half(~n), kind(-n), negate(n));
    console.log("text", `${-n} ${+n} ${~n}`, typeof -n, typeof +n, typeof ~n);
    console.log("mixed", -n + n, -n * 3, 2 ** -n, 1 / -n, ~n & 6, ~n >>> 28);
    if (n === 0) {
      console.log("narrowed", Object.is(-n, -0), ~n);
    } else {
      console.log("narrowed", -n, ~n);
    }
  }
  const signs = [-1, 0, 1];
  for (const s of signs) {
    console.log("sign", -s, +s, ~s, Object.is(-s, -0), -s / 2);
  }
  const five = 5;
  console.log("single", -five, +five, ~five, -five / 2, half(-five));
  const flag = new Flag();
  console.log("field", -flag.bit, +flag.bit, ~flag.bit, flag.flip(), -flag.bit / 2);
  const maybe = bits[1];
  if (maybe !== null) {
    console.log("option", -maybe, ~maybe, +maybe / 2);
  }
}

function floats(h, w) {
  console.log("float", -h, +h, ~h, -h * 2, typeof -h);
  console.log("float single", -w, +w / 2, ~w);
}

function sized(i, b) {
  console.log("sized", -i, +i, ~i, +b);
}

run();
floats(0.5, 2.5);
floats(1.5, 2.5);
sized(3, 1);
sized(-4, 2);
