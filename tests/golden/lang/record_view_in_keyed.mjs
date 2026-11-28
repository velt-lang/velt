// The Node twin of tests/golden/lang/record_view_in_keyed.velt; its .out is what Node 22 prints.

// `k in r` of a record view of a class instance in a program that defines a record's accessors
// (`Object.defineProperty`), where `in` tests a record's accessors too: what the instance
// inherits is still found, as in Node, and `Object.hasOwn` stays with its own keys (#3087).
class Product {
    name = "pen";
    label() {
        return this.name;
    }
}
function main() {
    const a = new Product();
    const r = a;
    const k = "label";
    console.log("label" in r, k in r, Object.hasOwn(r, "label"), "name" in r, "zz" in r);
    const plain = {};
    Object.defineProperty(plain, "n", { get: () => 1 });
    console.log("n" in plain, "m" in plain, "toString" in plain, plain.n);
}
main();
