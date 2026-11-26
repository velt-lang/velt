function head(s) {
  return (s ?? "").split("\n")[0];
}

class S1 extends Error {
  get stack() {
    return "S1-stack";
  }
  ownView() {
    return head(this.stack);
  }
}

class S2 extends Error {
  get stack() {
    return "S2-stack";
  }
}

class S3 extends S1 {
  get stack() {
    return "S3-stack";
  }
}

class T1 extends TypeError {
  get stack() {
    return "T1-stack";
  }
}

const s1 = new S1("s");
console.log(head(s1.stack), s1.ownView(), String(s1));
console.log(head(new S2("t").stack));
const s3 = new S3("u");
console.log(head(s3.stack), s3.ownView());
console.log(head(new T1("v").stack));

const all = [new S1("a"), new S2("b"), new S3("c"), new T1("d"), new Error("e")];
for (const e of all) console.log(head(e.stack), e.message, String(e));

try {
  throw new S2("thrown");
} catch (e) {
  if (e instanceof Error) console.log(head(e.stack));
}

class WithSetter extends Error {
  get stack() {
    return "WithSetter-get";
  }
  set stack(v) {
    console.log("WithSetter-set", v);
  }
}
const ws = new WithSetter("m");
ws.stack = "assigned";
console.log(head(ws.stack));

class SetterOnly extends Error {
  set stack(v) {
    console.log("SetterOnly-set", v);
  }
}
const so = new SetterOnly("m");
console.log(head(so.stack));
so.stack = "assigned too";
console.log(head(so.stack));

class InCtor extends Error {
  constructor(m) {
    super(m);
    this.stack = "custom\nframe";
  }
}
console.log(head(new InCtor("c").stack));
const later = new Error("plain");
later.stack = "set later";
console.log(head(later.stack));
