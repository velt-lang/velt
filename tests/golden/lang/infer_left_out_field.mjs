// The Node twin of tests/golden/lang/infer_left_out_field.velt; its .out is what Node 22 prints.

// A type parameter that an object literal argument offers no candidate for (it leaves out every
// optional field naming it, also in a nested literal) takes its default, its constraint or
// `unknown`, as TypeScript infers it, after any other argument that infers it (#3116). A field
// written as `undefined` offers `undefined`, one written as `null` offers `null`.
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
function later3(a, b, x) {
    return a.input ?? b.input ?? x;
}
function nested(o) {
    return typeof o.opts.input;
}
function nestedSibling(o) {
    return o.opts.input ?? o.v;
}
function undef(o) {
    return [];
}
function both(o) {
    return o.input ?? o.v;
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
    // The later argument's type, not `unknown`: its methods and operators apply.
    console.log(later({}, 7).toFixed(1), later({}, "s").toUpperCase(), later3({}, {}, 2) + 1);
    console.log(nested({ opts: {} }), nested({ opts: { input: 5 } }), nestedSibling({ opts: {}, v: 4 }) + 1);
    const none = undef({ input: undefined });
    const three = undef({ input: 3 });
    console.log(none.length, three.length, both({ input: undefined, v: 3 }) + 1);
    console.log(undef({ input: undefined }).length, count({ input: undefined }));
    console.log(undef({ input: null }).length, count({ input: null }));
    load({}).then((s) => console.log("load", s));
}
main();
