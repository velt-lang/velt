// Node twin of any_cast_never_generic.velt (#3121): prints any_cast_never_generic.out.
const raw = { tag: "t", v: 1 };
const b = raw;
console.log(b.tag);
const n = raw;
console.log(n.tag, n.v);
