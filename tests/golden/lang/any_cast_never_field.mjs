// Node twin of any_cast_never_field.velt (#3121): prints any_cast_never_field.out.
function tagOf(w) {
  return w.tag;
}

const raw = { tag: "t", n: { foo: 1 } };
const w = raw;
console.log(w.tag);
console.log("after");

const o = { w: raw, k: 2 };
console.log(o.w.tag, o.k);

const list = [raw, { tag: "u", n: 0, m: "x" }];
console.log(list.length, tagOf(list[0]), tagOf(list[1]));

const { tag } = list[1];
console.log(tag);

const typed = { tag: "v" };
console.log(typed.tag);

const c = { ...w };
console.log(c.tag, w === w);
const opt = w;
console.log(opt !== undefined);
async function later(x) {
  return x.tag + "!";
}
later(w).then((s) => console.log(s));
