// The Node twin of tests/golden/lang/record_view_spread_wider.velt; its .out is what Node 22 prints.

// A record view (an interface with named members next to its index signature) is its record for
// inference: a spread of it into a record of a wider value type, and a generic function taking a
// `Record<string, V>`, as tsc accepts them (#3091).
function keysOf(r) {
    return Object.keys(r).join(",");
}
function keysOrNone(r) {
    return r == null ? "-" : Object.keys(r).join(",");
}
function main() {
    const meta = { name: "d", lang: "en" };
    const r = { ...meta };
    const w = { ...meta };
    const extra = { ...meta, n: 1 };
    w.flag = true;
    console.log(Object.keys(r).join(","), Object.keys(w).join(","), Object.keys(extra).join(","));
    console.log(w.name, w.flag, meta.flag, extra.n);
    console.log(keysOf(meta), keysOrNone(meta), keysOrNone(null));
}
main();
