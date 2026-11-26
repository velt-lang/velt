// Node twin of json_replacer_cycles.velt: prints its .out.

class Node1 {
  constructor(name) {
    this.name = name;
    this.kids = [];
    this.parent = null;
  }
}

class Base {
  tag = "b";
  other = null;
}

class Derived extends Base {
  extra = 1;
}

function logged(k, v) {
  console.log(`  replacer ${JSON.stringify(k)}`);
  return v;
}

function noParent(k, v) {
  return k === "parent" ? undefined : v;
}

function keep(k, v) {
  return v;
}

function viewed(k, v) {
  console.log(`  replacer ${JSON.stringify(k)} ${JSON.stringify(v)}`);
  return v;
}

function attempt(what, f) {
  try {
    console.log(what, f());
  } catch (e) {
    console.log(what, e instanceof TypeError ? "TypeError" : "other");
  }
}

const root = new Node1("root");
const kid = new Node1("kid");
root.kids.push(kid);
kid.parent = root;
attempt("tree, logged:", () => JSON.stringify(root, logged));
attempt("tree, no parent:", () => JSON.stringify(root, noParent));
const d = new Derived();
d.other = d;
attempt("derived in a base field:", () => JSON.stringify(d, keep));
attempt("derived, dropped:", () => JSON.stringify(d, (k, v) => (k === "other" ? undefined : v)));
const a = new Base();
const b = new Base();
a.other = b;
b.other = a;
attempt("two objects:", () => JSON.stringify(a, keep));
const l = { id: 1, next: [] };
l.next.push(l);
attempt("through an array:", () => JSON.stringify(l, keep));
attempt("element left out:", () => JSON.stringify(l, (k, v) => (k === "0" ? undefined : v)));
const shared = new Base();
attempt("same value twice:", () => JSON.stringify([shared, shared], keep));
const arr = [1];
arr.push(arr);
attempt("any array:", () => JSON.stringify(arr, keep));
attempt("any array, left out:", () => JSON.stringify(arr, (k, v) => (k === "1" ? undefined : v)));
attempt("tree again:", () => JSON.stringify(root, noParent));
console.log(JSON.stringify({ at: new Date(0), n: 1 }, viewed));
