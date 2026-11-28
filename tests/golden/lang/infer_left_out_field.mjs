// The Node twin of tests/golden/lang/infer_left_out_field.velt; its .out is what Node 22 prints.

function count(opts) {
    return typeof opts.input;
}
function bounded(opts) {
    return typeof opts.input + opts.n;
}
function defaulted(opts) {
    return [];
}
function later(opts, x) {
    return opts.input ?? x;
}
function describe(o) {
    return o.name + ":" + typeof o.value;
}
class Box {
    put(o) {
        return "put " + typeof o.v;
    }
}
async function load(o) {
    return typeof o.result;
}
function main() {
    console.log(count({ input: 1 }), count({}), count({ input: "s" }));
    console.log(bounded({ n: 3 }), bounded({ input: 5, n: 3 }));
    const ds = defaulted({});
    ds.push("x");
    const dn = defaulted({ input: 2 });
    dn.push(4);
    console.log(ds.length, dn[0]);
    console.log(later({}, 7), later({ input: 1 }, 2));
    const l = later({}, 4);
    console.log(l + 1);
    console.log(describe({ name: "a" }), describe({ name: "b", value: true }));
    console.log(new Box().put({}), new Box().put({ v: 1 }));
    load({}).then((s) => console.log("load", s));
}
main();
