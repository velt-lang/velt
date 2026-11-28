// The Node twin of tests/golden/lang/record_view_in_inherited.velt; its .out is what Node 22 prints.

// `k in r` of a record view of a class instance finds what the instance inherits (its class's
// methods and accessors, a base class's, `Object.prototype`'s) as Node's `in` does, while
// `Object.hasOwn` stays with its own keys (#3087): a literal key and a key known only at run time.
class Base {
    kind() {
        return "b";
    }
}
class Product extends Base {
    name = "pen";
    label() {
        return this.name;
    }
    get title() {
        return "T";
    }
    static make() {
        return 1;
    }
}
function main() {
    const a = new Product();
    const r = a;
    const k = "label";
    console.log("label" in r, Object.hasOwn(r, "label"), k in r);
    console.log("kind" in r, "title" in r, "make" in r, "name" in r, "nope" in r);
    console.log("toString" in r, Object.hasOwn(r, "toString"));
    for (const key of ["label", "name", "kind", "zz"]) {
        console.log(key, key in r, Object.hasOwn(r, key));
    }
    const plain = { x: 1 };
    console.log("toString" in plain, "x" in plain, "label" in plain);
    const o = { y: 2, f() { return 1; } };
    const ro = o;
    console.log("f" in ro, "y" in ro, "label" in ro, Object.hasOwn(ro, "f"));
}
main();
