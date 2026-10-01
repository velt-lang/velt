# Memory without a garbage collector

JavaScript runtimes reclaim memory with a garbage collector: objects live until the collector
finds them unreachable, which costs memory headroom and pauses. Velt has **no garbage
collector**. Memory is freed at a known point, when its owner goes away, so there are no
collection pauses, no heap to tune, and cleanup code runs exactly when you expect.

You don't manage memory by hand either: there is no `free`, no lifetime annotation, no `mut`,
no borrow syntax. The compiler infers all of it. This page explains the model as it is today
and what changes next.

## Values: numbers, strings and Copy structs

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
text. Structs whose fields are all numbers, booleans or other such structs are copied too.

## Objects have one owner (today)

Arrays, maps, class instances and object literals live on the heap and have **one owner**: the
variable, field or array element that holds them. When the owner goes out of scope, the object
is freed, together with everything it owns.

```ts
class Order {
  items: string[] = [];
}

function main() {
  const order = new Order();       // `order` owns the object
  order.items.push("tea");
  console.log(order.items.length); // 1
}                                  // freed here, deterministically
```

**Calls borrow.** Passing an object to a function lends it for the duration of the call, so the
usual JavaScript code just works:

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

The compiler infers, for every parameter, whether the function only reads it, modifies it, or
keeps it (stores or returns it). Only a function that keeps a parameter takes ownership.

**Assigning moves.** Where this model differs from JavaScript today: assigning an object to a
second variable, storing it in another object, or returning it **moves** it. The old variable
can't be used afterwards.

```ts error
class Box {
  v: i64 = 1;
}

function main() {
  const a = new Box();
  const b = a;              // ownership moves to b
  console.log(a.v);         // error: use of moved value `a`
}
```

The error shows where the value moved and lists the fixes: pass `a` to a function instead
(calls borrow), copy it with `a.clone()` (an independent deep copy), or share it with
`shared(a)`.

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

That rule is what lets the compiler assume no aliasing, as Rust does, and generate code that
keeps values in registers.

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

## Coming next: JavaScript's object semantics

The move rule is the one place where today's Velt asks TypeScript developers to think
differently. The next stage of the memory model removes it:

- **Semantics stage 2** (planned): objects, arrays, maps and closures become **shared
  references**, exactly like in JavaScript: `const b = a; b.push(1)` changes `a`, and "use of
  moved value" disappears. `.clone()` becomes an explicit deep copy, like `structuredClone`.
  Closures may modify the variables they capture even when they escape. Values that the
  compiler can prove have a single owner (the common case, and every hot loop in the benchmark
  suite) keep today's code with no reference counting; only values that are actually shared get
  a reference count. Each stage must keep every benchmark within 3% of the previous compiler.
- **Semantics stage 3** (planned): `weak` references for back-pointers (`parent: weak Node |
  null`) and a compile-time warning when a type can form a reference cycle. There is still no
  cycle collector.

What does not change: no garbage collector, no pauses, deterministic cleanup, compile-time
thread safety. The design and its rationale are in
[JavaScript semantics without a GC](../internals/design/semantics.md).

## Performance

Deterministic memory is part of how Velt matches Rust's speed. In the micro-benchmarks of
[bench/RESULTS.md](../../bench/RESULTS.md) (LLVM release builds against `rustc -O` and Node 22
on an Intel i9-12900HK under Windows 11, best of 10, ±15% noise), Velt runs the `hashmap`
benchmark in 182 ms (Rust 287 ms with std's SipHash `HashMap`, Node 479 ms) and `classes` in
201 ms (Rust 293 ms with `Box<dyn Trait>`, Node 521 ms). In the `bench/async` suite an idle Velt process uses about 50 MB less than Node.
