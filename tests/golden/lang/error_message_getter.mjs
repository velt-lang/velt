// `get message()` on a class extending `Error` (#3792), as in JavaScript: `Error`'s constructor
// gives an error its own `message` only when it is passed one (`""` included), and a read finds
// that first, else the getter (`Error.prototype.message` replaced). Every way a program reads it
// agrees: a member read, `String(e)`, `stack`, through `Error`, an error union, a type parameter,
// `any`, destructuring and optional chaining. Only the first line of `stack` is printed: Node adds
// the call frames after it.

function head(s) {
  return (s ?? "").split("\n")[0];
}

let calls = 0;

class E extends Error {
  get message() {
    calls++;
    return "[" + super.message + "]";
  }
}

// A subclass of a subclass: its getter wins, and `super.message` is its base's getter.
class F extends E {
  get message() {
    return "F:" + super.message;
  }
}

// A subclass of one with the getter, without its own: the inherited getter.
class G extends E {}

// Explicit constructors: `super()` and `super(m)` with `m` left out give no own message.
class C1 extends E {
  constructor() {
    super();
  }
}
class C2 extends E {
  constructor(m) {
    super(m);
  }
}

// A built-in error class's subclass: `new TypeError()` gives no own message either.
class T extends TypeError {
  get message() {
    return "T getter";
  }
}

// A class with no getter in a program that has one: as before.
class Plain extends Error {}

const given = new E("given");
const none = new E();
console.log(given.message, "|", none.message, "|", calls);
console.log(String(given), "|", String(none), "|", `${none}`);
console.log(head(given.stack), "|", head(none.stack));

// An own empty message is a message: the getter is not called.
const before = calls;
const empty = new E("");
console.log(JSON.stringify(empty.message), String(empty), calls - before);

// `undefined` is no message.
console.log(new E(undefined).message);

console.log(new F().message, "|", new F("own").message, "|", String(new F()));
console.log(new G().message, "|", new G("own").message);
console.log(new C1().message, new C2().message, new C2("two").message, new C2(undefined).message);
console.log(new T().message, "|", new T("t").message, "|", String(new T()));
console.log(JSON.stringify(new Plain().message), new Plain("p").message, String(new Error()));

// Through `Error`, and an array of errors of several classes.
const asError = none;
console.log(asError.message, asError instanceof E);
const errors = [new E(), new Error(), new TypeError(), new F("ff"), new Error("plain")];
for (const x of errors) {
  console.log(JSON.stringify(x.message), String(x));
}

// Writing through `Error` an error that has its own message: the getter stays unreached.
const written = new E("x");
written.message = "";
console.log(JSON.stringify(written.message), Object.hasOwn(given, "message"));

// A caught error union, a type parameter, optional chaining, `any`.
function thrower(n) {
  if (n > 0) {
    throw new E();
  }
  throw new TypeError("t");
}
for (const n of [1, 0]) {
  try {
    thrower(n);
  } catch (err) {
    console.log("caught", err.message, `${err}`);
  }
}
function messageOf(x) {
  return x.message;
}
console.log(messageOf(none), messageOf(given), messageOf(new F()));
const maybe = calls > 1000 ? null : none;
console.log(maybe?.message);
const held = none;
const heldOwn = given;
console.log(held.message, heldOwn.message, String(held));

// Destructuring: a declaration, a parameter, an assignment.
const { message, name } = none;
function messageParam({ message }) {
  return message;
}
let assigned = "";
({ message: assigned } = new F());
console.log(message, name, messageParam(none), messageParam(given), assigned);

// A closure and a template.
const read = () => none.message;
console.log(read(), `${none.message}!`, none.message.length);
