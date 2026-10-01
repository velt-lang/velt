# Memory without a garbage collector

JavaScript runtimes reclaim memory with a garbage collector: objects live until the collector
finds them unreachable, which costs memory headroom and pauses. Velt has **no garbage
collector**. Memory is freed at a known point, when its last reference goes away, so there are
no collection pauses, no heap to tune, and cleanup code runs exactly when you expect.

You don't manage memory by hand either: there is no `free`, no lifetime annotation, no `mut`,
no borrow syntax. The compiler infers all of it, and the code you write behaves as it does in
JavaScript.

## Values: numbers and strings

Numbers, booleans and strings behave exactly as in JavaScript: assigning one copies it, and the
copy is independent.

```ts
let a = "tea";
let b = a;            // a copy
b += " with milk";
console.log(a, b);    // tea tea with milk
```

Strings are immutable values. Short ones (up to 23 bytes) live inline with no allocation;
longer ones share one reference-counted buffer, so a copy is cheap and never duplicates the
text.

## Objects are references

Arrays, maps, class instances, object literals and closures are **references**, as in
JavaScript: assigning an object to a second variable, storing it in another object or returning
it refers to the same object. `.clone()` makes an independent deep copy, like
`structuredClone`, and `==` compares objects by identity (`deepEqual` compares contents).

```ts
class Box {
  v: i64 = 1;
}

const a = new Box();
const b = a;          // the same object
b.v = 2;
const c = a.clone();  // an independent deep copy
c.v = 3;
console.log(a.v, c.v, a == b, a == c);   // 2 3 true false
```

An object is freed the moment its last reference goes, together with everything only it
refers to:

```ts
class Order {
  items: string[] = [];
}

function main() {
  const order = new Order();       // the only reference
  order.items.push("tea");
  console.log(order.items.length); // 1
}                                  // freed here, deterministically
```

**Calls borrow.** Passing an object to a function lends it for the duration of the call, and the
function sees the same object, so changes are visible to the caller:

```ts
class User {
  name: string = "ann";
  visits: i64 = 0;
}

function greet(u: User): string {
  return `hi ${u.name}`;
}

function recordVisit(u: User) {
  u.visits++;                      // the caller sees this, as in JS
}

const user = new User();
console.log(greet(user));          // `user` is lent, not given away
recordVisit(user);
recordVisit(user);
console.log(user.visits);          // 2
```

**What it costs.** The compiler infers, for every parameter, whether the function only reads
it, modifies it, or keeps it (stores or returns it), and for every value whether it ever has
more than one owner. A value with a single owner, the common case and every hot loop in the
benchmark suite, is moved with no reference count, exactly like Rust code. Only types whose
values the program actually shares get a reference count: one word in front of the object and
an increment when it is shared. Closures work the same way: a variable that a stored closure
and its enclosing function both change lives in a small shared cell, so both see every change.

## Mutation is inferred

There is no `mut` or `readonly` parameter syntax to write. The compiler infers which functions
modify their parameters and which methods modify `this`, across the whole program, and uses that
to optimize. One rule follows from it: a call may not modify a value that another of its
arguments can also reach.

```ts error
function append(a: i64[], b: i64[]) {
  for (const x of b) {
    a.push(x);
  }
}

function main() {
  const xs = [1, 2];
  append(xs, xs);           // error: this call may modify `xs` through another argument
}
```

That rule is what lets the compiler assume no aliasing for values with one owner, as Rust does,
and generate code that keeps values in registers. Two variables that refer to the same array
(`const ys = xs; append(xs, ys)`) are fine and behave as in JavaScript: that array type is then
reference-counted and the compiler makes no such assumption for it.

## Sharing across tasks

Data used by several tasks at once lives in `shared(...)`: an atomically reference-counted
value, freed when the last reference goes away. A `Mutex` inside guards changes. See
[Async and concurrency](async.md#sharing-state-between-tasks).

## Cleanup runs at a known point

Because freeing is deterministic, a class can define `[Symbol.dispose]()` (TypeScript's cleanup
method) and rely on it running when the value is freed. `using` makes the point explicit: at
the end of the block, on every exit path, in reverse order.

```ts
class Connection {
  addr: string;

  constructor(addr: string) {
    this.addr = addr;
  }

  [Symbol.dispose]() {
    console.log(`closed ${this.addr}`);
  }
}

function ping(addr: string): string {
  using conn = new Connection(addr);
  if (addr == "") {
    return "no address";       // conn is closed here too
  }
  return `PONG from ${conn.addr}`;
}                              // conn is closed here

console.log(ping("db:5432"));
// closed db:5432
// PONG from db:5432
```

`await using` does the same with an async `[Symbol.asyncDispose]()`.

## Reference cycles

Reference counting can't free a cycle: if `a.next = b` and `b.next = a`, neither count reaches
zero, and both objects stay allocated until the program exits. Break the cycle by hand (set one
link to `null`) when you are done with such a structure. **Planned**
([semantics stage 3](../internals/design/semantics.md#reference-cycles--without-a-collector)):
`weak` references for back-pointers (`parent: weak Node | null`) and a compile-time warning
when a type can form a reference cycle. There is still no cycle collector.

The design and its rationale are in
[JavaScript semantics without a GC](../internals/design/semantics.md).

## Performance

Deterministic memory is part of how Velt matches Rust's speed. In the micro-benchmarks of
[bench/RESULTS.md](../../bench/RESULTS.md) (LLVM release builds against `rustc -O` and Node 22
on an Intel i9-12900HK under Windows 11, best of 10, ±15% noise), Velt runs the `hashmap`
benchmark in 182 ms (Rust 287 ms with std's SipHash `HashMap`, Node 479 ms) and `classes` in
201 ms (Rust 293 ms with `Box<dyn Trait>`, Node 521 ms). In the `bench/async` suite an idle Velt process uses about 50 MB less than Node.
