// The Node twin of tests/golden/std/regexp_print.velt; its .out is what Node 22 prints.

class Sub extends RegExp {}

const r = /a/g;
console.log(r);
console.log([r, /b/]);
console.log({ r, n: 1 });
console.log(new Sub("x", "y"));
console.log([new Sub("x", "")]);
console.log(`${r}`, String(/c/im), r.toString());

const patterns = ["a/b", "", "\n", "[/]", "\\/", "a b", "\\\nz", "x\ry/"];
for (const p of patterns) {
  const re = new RegExp(p, "g");
  console.log(JSON.stringify(re.source), `${re}`);
  console.log(re.test(`-${p}-`) === new RegExp(re.source, "g").test(`-${p}-`));
}
console.log(new RegExp("a", "yigms").flags, /a/dg.flags);
