# Memory model

There is no garbage collector, and nothing ever pauses the program: memory is freed
deterministically when the last reference to it goes away. The compiler infers ownership and
mutation, so programs carry no lifetime, borrow or `mut` annotations. For a gentler
explanation, see [Memory without a garbage collector](../book/memory.md).

## Values and references

- **Values that copy**: numbers, `bool`, strings ([Strings](types.md#strings)), literal and
  enum types, and unions, tuples and `T | null` of those. A tuple is copied when it is
  assigned or stored, but a function or callback that changes an element of a tuple it was
  passed (`t[1] = 0`) changes the caller's tuple, as in JS.
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
  then stored behind a pointer). Freeing is deterministic. An array type iterated through an
  `Iterable<T>` (a value or a bound: `sum(xs)` with `sum(xs: Iterable<i64>)`) counts as shared
  for the whole program, since its iterator holds the array; nothing else about it changes.
  Naming an object again inside one function does not share it: `const me = this`, or a
  closure that captures `this` and is only called where it is created
  ([Captures](functions.md#captures)), refers to the same object without a count (unless a
  conflict elsewhere in the program makes them share; see [Captures](functions.md#captures)).
- **Calls borrow**: passing an object to a function lends it, so `log(user); save(user);` costs
  nothing. The compiler infers per parameter whether the callee reads it, modifies it, or keeps
  it. A parameter the body stores or returns takes ownership: a caller that does not use its
  variable again hands it over for free, one that does shares it. Returning a field of a class
  instance (a getter's `return this.ctl.sig`) shares the field and only borrows the instance.
- A **promise** has one owner: `await` a stored promise once; using a promise variable after
  handing it on is ``use of moved value `p` ``.
- Dropping a long chain of objects never overflows the stack, however it is linked: a
  linked list through a `next: Node | null` field or a tree through `left` and `right` is
  freed in a loop (when the class is not part of a class hierarchy, with no base class and no
  subclasses, and no field declared after the link can run a `[Symbol.dispose]()`), and any other chain (through arrays, `Map` values, closures,
  interface values, subclasses, struct values or a recursive object type such as `interface
  Node { next?: Node }`) is freed in nested steps up to a fixed depth (128 levels of such
  nesting), with the objects past it freed when the outer drop is done
  ([order of cleanup](#order-of-cleanup-in-long-chains)).
- Reference cycles (`a.next = b; b.next = a`) are never freed, and that includes an object
  holding a closure that captured it (`this.onChange = () => this.render()` in a constructor
  or method: the closure refers to the object, the object to the closure); replace the field
  (`this.onChange = () => {}`) when the object is done to free both. **Planned**
  ([semantics — cycles](../internals/design/semantics.md#reference-cycles--without-a-collector)):
  `weak` fields and a compile-time warning for reference cycles.
- `WeakMap`, `WeakSet` and `WeakRef` ([prelude](../std/prelude.md#weakmap-weakset-and-weakref))
  don't keep their keys and targets alive: an object's entries go when its last reference does,
  at once (Node waits for a garbage collection). A weak map whose value refers back to its key,
  such as a cache from an object to a wrapper holding it (`cache.set(raw, new Proxy(raw))`),
  frees both once nothing else refers to either, like JavaScript
  ([design](../internals/design/weak-refs.md#the-ephemeron-rule)). Not freed: a value graph of
  more than 256 objects, and a key that is also in an ordinary cycle (`raw.self = raw`).
  Releasing an object of a type that can be a weak key, or be reached from a weak map's keys and
  values, costs two more instructions when it frees the object; other types are unaffected.
- **Evaluation order is JS's**: operands and arguments run left to right, and one read before a
  later operand's call keeps the value it had (`f(o.v, o.change())` passes the old `o.v`). The
  target of an assignment is evaluated before its right-hand side: a call at its root runs
  once, first (`get().v = f()`, `m.get(k)!.v = f()`). An assignment or compound assignment to
  a field of a class object reached through a place writes to the object its target named
  before the right-hand side ran: in `o.inner.v = f()` or `o.inner.v += f()`, when `f`
  replaces `o.inner`, the old object gets the value and the new one keeps its own, as in JS.
  This holds however deep the target is (`o.a.b.c.v = f()` when `f` replaces `o.a` and
  something else still refers to the old `o.a`). An old object nothing else refers to is freed
  by `f`, and the write is dropped with it. **Known differences:** when `f` frees the old
  object and then installs a new one at the same place (`o.inner = null; o.inner = new P()`),
  the allocator may give the new object the freed address; the write then goes to the new
  object, where Node writes to the unreachable old one
  ([#825](https://github.com/velt-lang/velt/issues/825)). A target in an object literal
  (`o.inner.v = f()` with `o = { inner: { v: 1 } }`) is written in the object the right-hand
  side installed ([#876](https://github.com/velt-lang/velt/issues/876)), and so is an array
  element whose array the right-hand side replaces
  ([#820](https://github.com/velt-lang/velt/issues/820)).

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
`m.with(f)` gives `f` the locked value; when `m` is a `shared` handle, that value is not part of
`m`'s place, so a callback may read what sits next to it (`this.m.with((v) => { v.n +=
this.step; })`), while one that changes the object holding `m` is still an error.

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
variable stays put until then: passing it to a function that borrows it is fine, and so is
an async call awaited where it is made (`await x.read()`), but moving it away (returning it,
storing it, a stored or returned promise of an async call on it, `spawn`, an explicit
`x[Symbol.dispose]()`) is an error; declare it with `const` for that. `using` is only allowed inside blocks.

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

### Order of cleanup in long chains

When an object goes away, its `[Symbol.dispose]()` runs first, then its fields are released in
declaration order, each completely (an object a field held is disposed with everything it
holds) before the next. A chain through the class's own field (`next: Node | null`) keeps
exactly this order at any length (each node is disposed before the rest of the chain) when that
field is declared after every field whose drop can run a `[Symbol.dispose]()`, and the class
is not part of a class hierarchy (no base class and no subclasses); otherwise it is released in nested steps like the chains below. In a chain
or tree that nests through other values (arrays, `Map` values, closures, interfaces, recursive
object types), an object more than 128 such levels below the one being released is set aside
and released, in the order it was reached, once the outer release has finished everything
else. Every hook still
runs exactly once, and a chain is still disposed from its head on; only a branch deeper than
128 levels is disposed after the shallower objects that come after it.

```ts
class Step {
  name: string;
  next: Step[] = [];

  constructor(name: string) {
    this.name = name;
  }

  [Symbol.dispose]() {
    console.log("dispose", this.name);
  }
}

{
  const a = new Step("a");
  const b = new Step("b");
  b.next.push(new Step("c"));
  a.next.push(b);
  a.next.push(new Step("d"));
}                               // dispose a, dispose b, dispose c, dispose d
```

Not supported yet: the `Disposable` / `AsyncDisposable` interfaces and `DisposableStack` (a
`using` value needs a class or struct type with the method), `using` in `for (… of …)` heads,
and calling `[Symbol.dispose]()` through an interface or a type parameter (the value's own drop
would run the hook again).
