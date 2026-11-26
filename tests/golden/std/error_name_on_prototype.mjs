// The Node twin of error_name_on_prototype.velt.

function first(s) {
  return String(s).split("\n")[0];
}

class R1 extends TypeError {
  get name() {
    return "R1";
  }
}
class R2 extends RangeError {
  get name() {
    return "R2";
  }
}
class R3 extends SyntaxError {
  get name() {
    return "R3";
  }
}
class R4 extends ReferenceError {
  get name() {
    return "R4";
  }
}
class R5 extends EvalError {
  get name() {
    return "R5";
  }
}
class R6 extends URIError {
  get name() {
    return "R6";
  }
}
class R7 extends DOMException {
  get name() {
    return "R7";
  }
}

function show(e) {
  console.log(e.name, String(e), e.message, first(e.stack));
}

show(new R1("r1"));
show(new R2("r2"));
show(new R3("r3"));
show(new R4("r4"));
show(new R5("r5"));
show(new R6("r6"));
show(new R7("r7", "AbortError"));

class P extends RangeError {
  get name() {
    return "P";
  }
  set name(v) {
    console.log("setter called with", v);
  }
}
const p = new P("pp");
show(p);
p.name = "Q";

show(new TypeError("t"));
show(new URIError("u"));
const t = new TypeError("renamed");
t.name = "Renamed";
show(t);
const d = new DOMException("dm", "InvalidCharacterError");
console.log(d.name, d.code, String(d));

class App extends Error {
  constructor(m) {
    super(m);
    this.name = "AppError";
  }
}
console.log(first(new App("m").stack));
const late = new Error("late");
late.name = "Later";
console.log(first(late.stack));

function report(e, stack) {
  return first(stack).length + e.message.length;
}
const r = new Error("m");
console.log(report(r, r.stack));

const own = new Error("own");
own.stack = "custom stack";
console.log(own.stack);
const g = new Error("g");
g.stack = undefined;
console.log(g.stack === undefined, String(g));
