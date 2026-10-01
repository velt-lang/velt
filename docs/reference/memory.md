# Memory model

There is no garbage collector, and nothing ever pauses the program: memory is freed
deterministically when the last reference to it goes away. The compiler infers ownership and
mutation, so programs carry no lifetime, borrow or `mut` annotations. For a gentler
explanation, see [Memory without a garbage collector](../book/memory.md).

## Values and references

- **Values that copy**: numbers, `bool`, strings ([Strings](types.md#strings)), literal and
  enum types, and unions, tuples and `T | null` of those.
- **Objects are references**, as in JS: class instances, arrays, maps, structs, object types
  (`type P = { x: i64 }`, object literals) and closures. `const b = a;`, passing `a` to a
  function, storing it in a field, an array or a map, returning it and capturing it all refer
  to the *same* object: `b.push(1)` changes `a`. Elements, fields and `Map.get` results are the
  stored objects themselves: changing the object `m.get(k)` returns changes the one in the
  map. `x.clone()` makes an independent deep copy (like `structuredClone`).
- **No garbage collector, no pauses**: memory is freed (and `[Symbol.dispose]()` runs,
  [below](#resource-cleanup-using-and-symboldispose)) the moment the last reference goes. The
  compiler infers ownership: a value with a single owner is moved, with no reference count and
  the same code as Rust, and only types whose values the program actually shares get a
  reference count (a word in front of the object; arrays and object types that are shared are
  then stored behind a pointer). Freeing is deterministic.
- **Calls borrow**: passing an object to a function lends it, so `log(user); save(user);` costs
  nothing. The compiler infers per parameter whether the callee reads it, modifies it, or keeps
  it. A parameter the body stores or returns takes ownership: a caller that does not use its
  variable again hands it over for free, one that does shares it.
- A **promise** has one owner: `await` a stored promise once; using a promise variable after
  handing it on is ``use of moved value `p` ``.
- Reference cycles (`a.next = b; b.next = a`) are never freed. **Planned**
  ([semantics — cycles](../internals/design/semantics.md#reference-cycles--without-a-collector)):
  `weak` references and a compile-time warning for reference cycles.

```ts
class Box {
  v: i64 = 1;
}

function bump(b: Box) {
  b.v += 1;
}

const a = new Box();
const b = a;                    // the same object
b.v = 5;
bump(a);
const copy = a.clone();         // a deep copy
copy.v = 0;
console.log(a.v, b.v, copy.v);  // 6 6 0
console.log(a == b, a == copy); // true false: `==` compares objects by identity
```

## Mutation is inferred

There is no `mut`; writing it is an error
(``` `mut` is not needed: mutation is inferred ```). Over the whole program the compiler infers:

- **Methods** that modify `this`: directly, through a field, through a callee or a closure, on
  any path. An overridden method and its overrides share the answer, as do an interface
  method's implementations.
- **Parameters** whose contents are modified (field writes, `push`, modifying methods, passing
  them on). The caller sees those changes; reassigning the parameter only rebinds the local
  name. Number, bool and string parameters are local copies.
- **Callbacks** receive objects by reference and may modify them; calling a function value of
  unknown behavior counts as modifying the call's other arguments.
- Not allowed: modifying a module constant through a call
  (``cannot modify module-level constant `X` ``) and reassigning an object parameter of a
  closure or of an overridden or interface method.

```ts
class Cart {
  items: string[] = [];
}

function addItem(cart: Cart, item: string) {
  cart.items.push(item);        // modifies `cart`: the caller sees it
}

function bump(n: i64) {
  n += 1;                       // a copy: local change only
}

const cart = new Cart();
addItem(cart, "tea");
let n = 1;
bump(n);
console.log(cart.items, n);     // [ 'tea' ] 1
```

## Exclusive access

A value that a call may modify (a modified parameter, the receiver of a modifying method, a
place a closure argument modifies) must not be reachable through any other argument of the same
call: directly, as an overlapping place (`a` / `a.f`, `xs` / `xs[i]`), through a `for...of`
binding, or as a closure capture. `append(xs, xs)`, `append(g.rows[0], g.rows[1])` and
`xs.forEach((x) => { xs.push(x); })` are errors
(``cannot use `xs` here: this call may modify it through another argument``). Reading a number
is fine (`xs.push(xs.length)`), and distinct fields are disjoint (`f(this.a, this.b)`). This rule
is what lets the compiler assume no aliasing for values with a single owner, like Rust. Two
variables that refer to the same object (`const ys = xs; append(xs, ys)`) are allowed and work
as in JS: the type is then reference-counted, and the compiler makes no such assumption for it.

```ts error
function append(a: i64[], b: i64[]) {
  for (const x of b) {
    a.push(x);
  }
}

function main() {
  const xs = [1, 2];
  append(xs, xs);               // error: this call may modify `xs` through another argument
}
```

## Resource cleanup: `using` and `[Symbol.dispose]`

A class or struct may define `[Symbol.dispose](): void` (TypeScript's cleanup method name). It
runs automatically when the last reference to the object goes away (end of scope, overwrite,
owner dropped), before its fields are dropped, like Rust's `Drop`. Calling it explicitly,
`x[Symbol.dispose]()`, releases `x` right there (the hook runs once, when no other reference
remains; using `x` afterwards is ``use of moved value `x` ``). A method merely named `dispose()`
is an ordinary method. APIs with an explicit `close()` keep it, like Node.

```ts
class TempFile {
  path: string;

  constructor(path: string) {
    this.path = path;
  }

  [Symbol.dispose]() {
    console.log("removing", this.path);
  }
}

{
  const t = new TempFile("a.tmp");
  console.log("using", t.path);
}                               // prints "removing a.tmp" here
console.log("after");
```

**`using x = …;`** (TypeScript 5.2) declares a `const` that is disposed at the end of the
enclosing block: in reverse declaration order, and also on `return`, `break`, `continue` and
`throw`. The value's type must have `[Symbol.dispose]()`; a `null` value is skipped. A `using`
variable stays put until then: passing it to a function that borrows it is fine, but moving it
away (returning it, storing it, an explicit `x[Symbol.dispose]()`) is an error; declare it with
`const` for that. `using` is only allowed inside blocks.

```ts
class Lock {
  name: string;

  constructor(name: string) {
    this.name = name;
  }

  [Symbol.dispose]() {
    console.log("unlock", this.name);
  }
}

function transfer(fail: bool): string {
  using a = new Lock("a");
  using b = new Lock("b");
  if (fail) {
    return "aborted";           // unlock b, unlock a
  }
  return "done";                // unlock b, unlock a
}
```

**`await using x = …;`** (in async functions) awaits `x[Symbol.asyncDispose]()` at the end of
the block, in the same order as `using` and on the same exits. Without `[Symbol.asyncDispose]`
it falls back to `[Symbol.dispose]`, like TypeScript. `[Symbol.asyncDispose]` must be `async`,
take no parameters and return `Promise<void>`. Like every async method it takes `this` by
value, so it consumes the value (a `[Symbol.dispose]` hook of the same type runs when it
returns). An error thrown by the cleanup propagates from the block.

```ts
class Conn {
  addr: string;

  constructor(addr: string) {
    this.addr = addr;
  }

  request(line: string): string {
    return `${this.addr}: ${line}`;
  }

  async [Symbol.asyncDispose]() {
    await sleep(1);             // e.g. flush and say goodbye
    console.log("closed", this.addr);
  }
}

async function ping(addr: string): Promise<string> {
  await using conn = new Conn(addr);     // closed at the end of the block
  return conn.request("PING");
}
```

Not supported yet: the `Disposable` / `AsyncDisposable` interfaces and `DisposableStack` (a
`using` value needs a class or struct type with the method), `using` in `for (… of …)` heads,
and calling `[Symbol.dispose]()` through an interface or a type parameter (the value's own drop
would run the hook again).
