// Node twin of iface_downcast_union_both.velt: the same program with its types removed.
function areaOf(s) {
  return s.area();
}

const both = { area: () => 2, name: () => "b", sides: 3 };
const b = both;
const u = b;
console.log(u === undefined ? "none" : "converted", areaOf(b));
console.log(b.name(), b.sides);
const m = b;
console.log(m?.sides);
const n = b;
console.log(n?.name());
