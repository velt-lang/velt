// The table of function properties (std/lang/hooks/fn_props.velt, #2196): many functions with
// properties at once (the table grows, and removals keep the others reachable), many properties
// on one function (in insertion order, a deletion in the middle keeping the order), values
// replaced, a property holding a function with properties of its own, and a declared function
// whose properties live to the end.

function makeAll(n) {
  const fns = [];
  for (let i = 0; i < n; i++) {
    const f = () => i;
    f.id = i;
    f.label = "fn" + String(i);
    fns.push(f);
  }
  return fns;
}

// 512 functions with properties, then every other one let go: the others keep theirs.
let fns = makeAll(512);
const kept = [];
for (let i = 0; i < fns.length; i += 2) {
  kept.push(fns[i]);
}
fns = [];
let sum = 0;
for (const f of kept) {
  sum += f.id;
}
console.log(kept.length, sum, kept[5].label, kept[255].label, kept[255]());

// Many properties on one function, in insertion order.
const many = () => 0;
for (let i = 0; i < 12; i++) {
  many["k" + String(i)] = i * i;
}
console.log(Object.keys(many).join(","));
delete many.k3;
delete many.k0;
delete many.missing;
many.k5 = "five";
many.k12 = "new";
console.log(Object.keys(many).join(","), many.k5, many.k11, many.k3, "k3" in many);

// A property holding a function with properties of its own; the outer one let go first.
let outer = () => "outer";
const inner = () => "inner";
inner.depth = 2;
inner.text = "x".repeat(40);
outer.inner = inner;
outer.self = { name: "a record with a string long enough for the heap" };
console.log(outer.inner(), outer.inner.depth, outer.self.name.length);
outer = null;
console.log(inner.depth, inner.text.length);

// A declared function: every reference is one function, its properties kept to the end.
function declared() {
  return "declared";
}
const d1 = declared;
const d2 = declared;
d1.count = 1;
d2.count = d2.count + 1;
d1.note = "a note long enough to be made on the heap, not inline";
console.log(d1.count, d2.note.length, d1 === d2, Object.keys(d2).join(","));
