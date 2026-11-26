// The Node twin of tests/golden/std/typed_array_subclass_inspect.velt, without the types.
class V extends Int16Array {
  values() {
    console.log("values");
    return super.values();
  }
}

class W extends V {}

class Bytes extends Uint8Array {}

const v = new V(2);
v[1] = 5;
console.log(v);
console.log([v]);
console.log({ v: v });
console.log(new W(3));
console.log(new Bytes(0));
console.log(new Int16Array(2));
console.log({ a: { b: { c: v } } });
