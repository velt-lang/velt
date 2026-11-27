// The Node twin of tests/golden/std/regexp_define_last_index.velt; its .out is what Node 22
// prints.

class R extends RegExp {
  get lastIndex() {
    console.log("subclass getter");
    return 9;
  }
  set lastIndex(v) {
    console.log("subclass setter", v);
  }
}

const g = /a/g;
const back = Object.defineProperty(g, "lastIndex", { value: 2 });
console.log(back === g, g.lastIndex, g.exec("aaaa").index, g.lastIndex);

const r = new R("a", "y");
Object.defineProperty(r, "lastIndex", { value: 1 });
console.log(r.lastIndex, r.test("aaa"), r.lastIndex);
