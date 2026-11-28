// `s.split(re)` and `s.matchAll(re)` of a regex of a class that extends `RegExp` run on a copy
// made with the class's own constructor, as in Node (SpeciesConstructor, #3817): `new C(re,
// flags + "y")` for `split`, `new C(re, flags)` for `matchAll`. The constructor runs for the copy,
// `exec` is the copy's (its fields, its `flags` with the `y`), and the regex itself keeps its
// `lastIndex`. A class without a constructor of its own copies with `RegExp`'s, and one whose
// constructor takes no parameters gets no arguments (JavaScript drops them).
class Loud extends RegExp {
    calls = 0;
    constructor(p, f) {
        console.log("ctor", String(p), f);
        super(p, f);
    }
    exec(s) {
        this.calls++;
        console.log("exec on", this.flags, this.lastIndex, "call", this.calls, this instanceof Loud);
        return super.exec(s);
    }
}
class Quiet extends RegExp {
    exec(s) {
        console.log("quiet exec", this.flags, this.sticky);
        return super.exec(s);
    }
}
class Fixed extends RegExp {
    constructor() {
        console.log("Fixed ctor");
        super("b", "g");
    }
}
class Failing extends RegExp {
    made = 0;
    constructor(p, f) {
        super(p, f);
        if (typeof p !== "string") {
            throw new Error("no copies of " + String(p));
        }
    }
}
const loud = new Loud(",", "g");
console.log("a,b".split(loud), "calls", loud.calls, "lastIndex", loud.lastIndex);
loud.lastIndex = 2;
const found = [..."a,b,c".matchAll(loud)].map((m) => m[0] + "@" + m.index);
console.log(found, "lastIndex", loud.lastIndex);
console.log("x1y22".split(new Loud("\\d+", ""), 2));
console.log("a-b".split(new Quiet("-", "g")), [..."a-b-".matchAll(new Quiet("-", "g"))].length);
console.log("abcb".split(new Fixed()));
try {
    "a,b".split(new Failing(",", "g"));
}
catch (e) {
    console.log("caught", e.message);
}
