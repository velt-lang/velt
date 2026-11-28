// `slice`, `map`, `filter` and `subarray` of a typed array of a class that extends a typed array
// make their result with the class's own constructor, as in Node (TypedArraySpeciesCreate,
// #3817): `new C(length)`, or `new C(buffer, byteOffset, length)` for `subarray`, which shares the
// buffer. The result is of the class and keeps its overrides. `map` makes it before calling the
// callback, `filter` after. A plain typed array in the same program is not affected.
class Angled extends Int32Array {
    join(sep = ";") {
        return "<" + super.join(sep) + ">";
    }
}
class Loud extends Float64Array {
    constructor(a = 0, off, len) {
        console.log("Loud ctor", typeof a, off, len);
        if (typeof a === "number") {
            super(a);
        }
        else if (Array.isArray(a)) {
            super(a);
        }
        else {
            super(a, off, len);
        }
    }
}
class Halves extends Int16Array {
}
const p = new Angled([1, 2, 3]);
const s = p.slice(1);
console.log(s.join(), s instanceof Angled, s.length);
console.log(p.map((x) => x * 2).join(), p.filter((x) => x > 1).join(), p.subarray(1, 2).join());
const sub = p.subarray(1);
sub[0] = 9;
console.log(p.join(), sub instanceof Angled, sub.byteOffset);
const l = new Loud([1.5, 2.5, 3.5]);
console.log(l.slice(0, 2).join(), l.slice(5).length);
console.log(l.map((x) => {
    console.log("map", x);
    return x + 1;
}).join());
console.log(l.filter((x) => {
    console.log("filter", x);
    return x > 2;
}).join());
console.log(l.subarray(1).join(), l.subarray(1) instanceof Loud);
const h = new Halves([5, 6, 7]);
console.log(h.slice(1) instanceof Halves, h.map((x) => -x).join(), h.subarray(2) instanceof Halves);
const plain = new Int32Array([5, 6]);
console.log(plain.slice(1).join(), plain.map((x) => x + 1) instanceof Angled);
