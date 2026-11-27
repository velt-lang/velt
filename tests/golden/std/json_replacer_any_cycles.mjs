// Node twin of json_replacer_any_cycles.velt: prints its .out.

class C {
  a = 1;
  self = null;
}

class Tree {
  name;
  kids = [];
  constructor(name) {
    this.name = name;
  }
}

const c = new C();
c.self = c;
const arr = [1];
arr.push(arr);
const root = new Tree("root");
root.kids.push(new Tree("kid"));

function attempt(what, f) {
  try {
    console.log(what, f());
  } catch (e) {
    const kind = e instanceof TypeError ? "TypeError" : e instanceof RangeError ? "RangeError" : "other";
    console.log(what, kind);
  }
}

function dropSelf(k, v) {
  if (k === "") {
    return { a: c };
  }
  if (k === "self") {
    return undefined;
  }
  return v;
}

function keepSelf(k, v) {
  if (k === "") {
    return { a: c };
  }
  return v;
}

function logged(k, v) {
  console.log(`  replacer ${JSON.stringify(k)}`);
  if (k === "") {
    return { a: c, b: 2 };
  }
  if (k === "self") {
    return "cut";
  }
  return v;
}

function arrayDropped(k, v) {
  if (k === "") {
    return { list: arr };
  }
  if (k === "1") {
    return undefined;
  }
  return v;
}

function arrayKept(k, v) {
  if (k === "") {
    return { list: arr };
  }
  return v;
}

// The array being walked (its elements), returned again inside an element's result.
function upKept(k, v) {
  if (k === "0") {
    return { up: root.kids };
  }
  return v;
}

function upDropped(k, v) {
  if (k === "0") {
    return { up: root.kids };
  }
  if (k === "up") {
    return 0;
  }
  return v;
}

// A member's result that holds a cycle, itself inside a result with one.
function nested(k, v) {
  if (k === "") {
    return [c, 3];
  }
  if (k === "a") {
    return [arr, 4];
  }
  if (k === "self" || k === "1") {
    return undefined;
  }
  return v;
}

// A new cycle at every level: Node's stack overflow `RangeError`.
function endless(k, v) {
  const o = new C();
  o.self = o;
  if (k === "self") {
    return { a: 2, next: o };
  }
  return v;
}

function main() {
  attempt("dropped:", () => JSON.stringify({ x: 1 }, dropSelf));
  attempt("kept:", () => JSON.stringify({ x: 1 }, keepSelf));
  attempt("logged:", () => JSON.stringify({ x: 1 }, logged));
  attempt("array dropped:", () => JSON.stringify({ x: 1 }, arrayDropped));
  attempt("array kept:", () => JSON.stringify({ x: 1 }, arrayKept));
  attempt("holder kept:", () => JSON.stringify(root, upKept));
  attempt("holder replaced:", () => JSON.stringify(root, upDropped));
  attempt("nested:", () => JSON.stringify({ x: 1 }, nested));
  attempt("endless:", () => JSON.stringify(c, endless));
  attempt("after:", () => JSON.stringify({ x: 1 }, dropSelf));
  attempt("plain after:", () => JSON.stringify(root));
}

main();
