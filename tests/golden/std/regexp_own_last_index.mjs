const log = [];

class R extends RegExp {
  get lastIndex() {
    log.push("get");
    return 5;
  }
  set lastIndex(v) {
    log.push(`set ${v}`);
  }
}

const r = new R("a", "g");
console.log(r.exec("aa").index, r.lastIndex);
r.lastIndex = 0;
console.log(r.lastIndex, r.test("ba"), r.lastIndex);
const base = r;
base.lastIndex = 1;
console.log(base.lastIndex, r.exec("aba").index, r.lastIndex);
console.log("ab".replace(r, "x"), r.lastIndex, [..."aab".matchAll(r)].length);
console.log(log.length);
