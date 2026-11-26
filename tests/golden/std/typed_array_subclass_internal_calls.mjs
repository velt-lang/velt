// The Node twin of tests/golden/std/typed_array_subclass_internal_calls.velt, without the types.
class Picky extends Int32Array {
  slice(start = 1, end = Infinity) {
    console.log("slice", start);
    return super.slice(start, end);
  }

  values() {
    console.log("values");
    return super.values();
  }

  join(sep = ";") {
    return "<" + super.join(sep) + ">";
  }
}

const p = new Picky(3);
p[0] = 3;
p[1] = 1;
p[2] = 2;
console.log(p.toSorted().join(), p.toReversed().join(), p.with(0, 9).join());
console.log(p.toString(), String(p), `${p}`);
p.sort();
console.log(p.join(","));

class Halves extends Float64Array {
  slice(start = 1, end = Infinity) {
    console.log("slice", start);
    return super.slice(start, end);
  }

  values() {
    console.log("values");
    return super.values();
  }
}

const h = new Halves(3);
h[0] = 2.5;
h[1] = -0.5;
h[2] = 1.5;
console.log(h.toSorted().join(), h.toReversed().join(), h.with(2, 0.25).join());
h.sort();
console.log(h.join());

class Bytes extends Uint8Array {
  slice(start = 1, end = Infinity) {
    console.log("slice", start);
    return super.slice(start, end);
  }
}

const b = new Bytes(2);
b[0] = 200;
b[1] = 7;
console.log(b.toSorted().join(), b.toReversed().join(), b.with(1, 300).join());
