// Node twin of json_replacer_mutual.velt: prints its .out.

class Left {
  tag = "l";
  right = null;
}

class Right {
  tag = "r";
  left = null;
}

function logged(k, v) {
  console.log(`  replacer ${JSON.stringify(k)}`);
  return v;
}

function edited(k, v) {
  if (k === "n") return 10;
  if (k === "tag") return undefined;
  return v;
}

const a = { name: "a", b: { n: 1, a: { name: "inner", b: null } } };
console.log(JSON.stringify(a, logged));
console.log(JSON.stringify(a, edited));
const p = { q: { c: { p: { q: null } } } };
console.log(JSON.stringify(p, logged));
const l = new Left();
l.right = new Right();
l.right.left = new Left();
console.log(JSON.stringify(l, logged));
console.log(JSON.stringify(l, edited));
