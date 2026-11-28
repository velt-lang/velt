// Node twin of iface_downcast_union.velt: the same program with its types removed.
function describe(s) {
  const u = s;
  if ("name" in u) {
    return "named " + u.name() + " " + u.area();
  }
  return "sided " + u.sides + " " + u.area();
}

function maybe(s) {
  const m = s;
  return m === undefined ? "none" : m.name();
}

const n = { area: () => 1, name: () => "n" };
const q = { area: () => 4, sides: 4 };
console.log(describe(n));
console.log(describe(q));
console.log(maybe(n));
console.log(maybe(undefined));
const s = n;
const m = s;
console.log(m?.name());

const both = { area: () => 2, name: () => "b", sides: 3 };
console.log(describe(both));
const b = both;
console.log(b.sides);
