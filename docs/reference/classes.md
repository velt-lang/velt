# Classes, structs, interfaces and generics

Every object has a compile-time shape (a fixed layout), so a field access is one load: no
hidden classes and no runtime shape checks.

## Classes

- Fields need a type (`count: i64 = 0`), or an initializer that states one (`count = 0`,
  `done = false`, `items = new Map<string, i64>()`). A field without a default must be assigned
  in the `constructor`. `new C(…)` allocates the object on the heap and constructs it in
  JavaScript's order: a class's field initializers run once its base class is constructed
  (right after `super(…)` returns, or after the inherited constructor when the class has no
  constructor of its own), in declaration order, before the rest of its constructor body. So for
  `class D extends B`: the arguments, B's initializers, B's constructor body, D's initializers,
  D's constructor body. `new` throws whatever the initializers and constructors throw
  ([Errors](errors.md#throwing)); a constructor's `throws` clause covers the initializers it
  runs (those of its class and of the classes between it and the next constructor up).
  Initializers may construct their own class or classes whose initializers construct it back
  (`next: Node | null = more() ? new Node() : null`); a `new` in such a cycle throws what every
  class in the cycle's initializers throws. Before its initializer runs (for example in an
  overridden method that a base constructor calls) a field reads as zero or empty (`0`, `false`,
  `""`, an empty array); reading a field that holds an object there (a class instance, a `Map`) is
  unsound today ([#349](https://github.com/velt-lang/velt/issues/349)).
  A field declared from an integer literal (`count = 0`) holds a JS number, like
  `let count = 0` ([Numbers](types.md#numbers)).
- **Parameter properties**: `constructor(private readonly name: string, public age: i64) {}`
  declares the fields and assigns them, as in TypeScript (`protected` is accepted there and
  means public: there are no `protected` members). As TypeScript emits them (ES2022 class
  fields), they are the class's first fields, before the declared ones, which decides the key
  order of `console.log` and `JSON.stringify`; the constructor assigns them first (after
  `super(…)`), so they are set after the class's field initializers ran:

  ```ts
  function log(s: string): i64 {
    console.log(s);
    return 1;
  }
  class Point {
    label: i64 = log("label");
    constructor(public x: f64, public y: f64) {
      log("constructor");
    }
  }
  console.log(JSON.stringify(new Point(1, 2))); // label, constructor, {"x":1,"y":2,"label":1}
  ```
- **Private and protected constructors** (TypeScript's rules): `private constructor(…)` can be
  called (`new C(…)`) only inside the class body: its methods, static methods, field
  initializers and the closures in them. Such a class cannot be extended
  (``cannot extend `C`: its constructor is private``); a public static method is the usual way
  to create one. `protected constructor(…)` may also be called inside the bodies of subclasses
  (`super(…)` in their constructors, or `new C(…)`). Elsewhere `new C` is an error:
  ``the constructor of `C` is private: only the body of `C` can call `new C(...)` `` (or
  ``… is protected: only `C` and its subclasses can …``). A subclass without a constructor of
  its own inherits its base's, with the same rule. `JSON.parse<C>` (and `v.as<C>()`) cannot
  decode such a class, alone or inside another type, since decoding fills the fields without
  running a constructor; `JSON.stringify` writes it as usual
  ([`velt:json`](../std/json.md)).
- **Single inheritance**: `class B extends A`. The base's fields are a prefix of the subclass
  layout, so upcasts are free. Redefining a base method requires `override`; `super.m()` calls
  the base version. There are no abstract classes.
- **`super(…)`**: the constructor of a class whose base has a constructor calls `super(…)`, as
  in TypeScript. Statements may come before it as long as they don't use `this` or `super.x`
  (``'super' must be called before accessing 'this' in the constructor of a derived class``) or
  `return`; they run first, then the call's arguments, the base constructor, this class's field
  initializers and parameter properties, and the rest of the body:

  ```ts
  class Shape {
    constructor(public name: string) {}
  }
  class Square extends Shape {
    sides: i64 = 4;
    constructor(size: f64) {
      if (size <= 0) {
        throw new Error("size must be positive");
      }
      const name = `square ${size}`;
      super(name);
      console.log(this.name, this.sides); // square 2 4
    }
  }
  new Square(2);
  ```

  The call itself is a statement of the constructor's body, made once: not inside a block, `if`,
  `try`, `switch`, loop or closure, and not part of an expression. TypeScript requires this too
  once a class has initialized fields, parameter properties or private fields (TS2401), and
  otherwise allows a nested call. Velt requires it always: the field initializers run right
  after the call and every field must be initialized, so the call has to run exactly once on
  every path. This is the one place Velt is stricter than TypeScript here. When a constructor
  throws (before `super(…)`, in the base constructor or in a field initializer), `new` frees the
  object it allocated, dropping the fields that were set, without running the class's
  `[Symbol.dispose]()`: like JavaScript, nothing disposes an object `new` never returned.
- **Dispatch**: a method that is never overridden is called directly (and can be inlined). Only
  overridden methods go through a vtable, and only where the static type is a base class.
- **Members**: `private` (usable only inside the declaring type's body, including closures
  there, but not in subclasses: ``` `x` is private ```; the standard library's own modules may
  use the private members of its types, which is how std types build each other's handles;
  `console.log` shows private fields, as Node shows a TypeScript `private` field, except
  zero-sized ones such as std's `runtime` markers),
  `public` (the default), `readonly`
  fields (assignable only in the constructor), `static` methods, and
  `static readonly NAME: T = const;` constants (`Account.LIMIT`, `Math.PI`). Mutable statics
  and `protected` members don't exist (only a constructor can be `protected`).
- **Getters and setters**: `get size(): T { … }` is read as a property (`x.size`) and cannot be
  called or assigned; `set size(v: T) { … }` runs on `x.size = v`; with both, `x.size += 1` and
  `x.size++` use both. Implementations and overrides of a getter or setter must be accessors
  too. Getters cannot be `static` or `async`.
- Instances are references, as in JS ([Memory model](memory.md#values-and-references)):
  `const b = a` refers to the same object. `x.clone()` makes an independent deep copy of any
  class, struct or union (like `structuredClone`), except values owning a `[Symbol.dispose]`
  resource, which may define `clone()` themselves. Such a class's own `clone()` (no
  parameters, returning the class) is what every deep copy of it calls: `x.clone()` of an
  array or object holding it, and the copy a value gets when it goes to another task
  ([Async](async.md#promises)), which then deep-copies whatever the returned object still
  shares with the original. A class that owns no resource is deep-copied field by field
  wherever it is nested; its `clone()` runs only when called (`x.clone()` on it).
- **Symbol method names**: `[Symbol.iterator]()` and `[Symbol.asyncIterator]()` (the
  [iteration protocol](control-flow.md#iterables)), `[Symbol.dispose]()` and
  `[Symbol.asyncDispose]()` ([resource cleanup](memory.md#resource-cleanup-using-and-symboldispose))
  name methods as in TypeScript and are called as `x[Symbol.iterator]()`. Other symbols do not
  exist.
- Async methods take `this` by value: the promise owns it.
- An overridden method returning a promise reports its errors through the promise: when the
  base method or any override can fail, all of them must be `async`
  ([Async](async.md#errors)).
- A template literal calls a class's (or struct's) own `toString(): string`, as in JS
  (`` `total: ${price}` ``); without one it shows the value the way `console.log` does.

```ts
class Account {
  static readonly LIMIT: i64 = 1000;
  readonly owner: string;
  private balance: i64 = 0;

  constructor(owner: string) {
    this.owner = owner;
  }

  get total(): i64 {
    return this.balance;
  }

  deposit(amount: i64): bool {
    if (amount <= 0 || this.balance + amount > Account.LIMIT) {
      return false;
    }
    this.balance += amount;       // modifies `this`: inferred, no annotation
    return true;
  }
}

class Savings extends Account {
  rate: f64 = 0.02;

  constructor(owner: string) {
    super(owner);
  }

  override deposit(amount: i64): bool {
    return super.deposit(amount + 1);
  }
}

const a: Account = new Savings("ada");
const ok = a.deposit(100);      // dispatched to Savings.deposit
console.log(ok, a.total, a.owner, Account.LIMIT);
```

A private constructor with static factory methods, and a protected one for a base class:

```ts
class Celsius {
  private constructor(readonly degrees: f64) {}

  static of(degrees: f64): Celsius | null {
    return degrees < -273.15 ? null : new Celsius(degrees);
  }

  static fromFahrenheit(f: f64): Celsius {
    return new Celsius((f - 32.0) * 5.0 / 9.0);
  }
}

class Shape {
  protected constructor(readonly name: string) {}
}

class Square extends Shape {
  constructor(readonly side: f64) {
    super("square");
  }
}

console.log(Celsius.fromFahrenheit(212.0).degrees, Celsius.of(-300.0) == null); // 100 true
console.log(new Square(2.0).name);                                              // square
// new Celsius(5.0) and new Shape("x") are errors here; so is `class Kelvin extends Celsius`.
```

## Structs

`struct` declares an object type with the same members as a class (methods, getters,
`private`, `static`). It is built with a struct literal `Point { x: 1, y: 2 }` or an object
literal where a `Point` is expected. A struct has no constructor and no inheritance
(`implements` is allowed). Like every object it is a reference
([Memory model](memory.md#values-and-references)): assigning or passing it shares it. The
compiler stores it inline (no heap allocation, no count) as long as the program never shares a
value of the type, or never changes one in place.

**Planned** ([semantics](../internals/design/semantics.md#js-fidelity-decisions)): the `struct`
keyword is removed (use `class`, or `type Name = { … }` with `extend Name { … }` for plain data).

```ts
struct Vec2 {
  x: f64;
  y: f64;

  len(): f64 {
    return Math.sqrt(this.x * this.x + this.y * this.y);
  }
}

let p = Vec2 { x: 3.0, y: 4.0 };
const q = p;                    // the same object
p.x = 0.0;
console.log(p.len(), q.len());  // 4 4
```

## Interfaces

- An interface lists the methods, getters, setters and fields an implementing type must have,
  and may give methods **default bodies** (mixins). `class C implements I, J` takes any number;
  `interface B extends A, C` inherits everything.
- An interface with **methods** (or getters or setters, own or inherited) is nominal: a type
  implements it only by declaring `implements` (or through an [`extend`](#extend) block); an
  object literal does not satisfy one.
- An interface with **only fields** (at least one, no methods, extending only such interfaces)
  describes data: it is an object type, like `type User = { … }`, so object literals satisfy
  it, field reads are loads and it has a JSON form. It converts to and from object types with
  the same fields and stays the same object. `class C implements User` checks that the class
  has the fields, but a class instance is not a `User` (it is shared by reference; build one
  from its fields). As a bound, `<T extends HasId>` is satisfied by any type with the fields
  (a struct, class or object type, not generic). An interface may refer to itself through a
  field (`next?: Node`, `parent?: Category`, or a union such as `left: Tree`): such an object is
  stored behind a pointer, like a class instance, so its size is finite. Like JavaScript,
  `JSON.stringify` of an object that contains itself (`n.next = n`) fails: it panics with
  "converting circular structure to JSON"; `console.log` prints one like Node,
  `<ref *1> { value: 1, next: [Circular *1] }`. A struct that contains itself is still an error
  (``recursive type `S` has infinite size``): structs are stored inline.
- Used as a **generic bound** (`<T extends Named>`), an interface is resolved at compile time
  (direct calls). Used as a **value type** (`Named[]` holding different classes), it is a fat
  pointer (data plus vtable), like Rust's `dyn`.
- **Generic methods** (`apply<U>(f: (x: i64) => U): U[]`) are dispatched statically only: call
  them on a concrete class or on a `T extends I` generic, not on an interface value. Generic
  interface methods cannot have default bodies yet.
- A default body can be `async` (`async load(): Promise<T> { … }`), with the rules of an async
  class method ([Async](async.md#errors)). A method without a body cannot be: like in TypeScript,
  it declares a `Promise` result, and implementations may be `async`.

```ts
interface Named {
  name(): string;
  greet(): string {
    return `hi ${this.name()}`;           // default method
  }
}

class Dog implements Named {
  name(): string {
    return "rex";
  }
}

class Cat implements Named {
  name(): string {
    return "tom";
  }

  greet(): string {
    return "meow";
  }
}

function loudest<T extends Named>(x: T): string {   // monomorphized: no vtable
  return x.greet().toUpperCase();
}

const pets: Named[] = [new Dog(), new Cat()];       // interface values: dynamic dispatch
for (const p of pets) {
  console.log(p.greet());
}
console.log(loudest(new Dog()));
```

```ts
interface User {                          // only fields: an object type
  readonly id: i64;
  name: string;
  email?: string;
}

interface HasId {
  id: i64;
}

function idOf<T extends HasId>(x: T): i64 { // any type with an `id: i64` field
  return x.id;
}

const u: User = { id: 1, name: "ann" };   // an object literal satisfies it
console.log(JSON.stringify(u), idOf(u));
```

## Generics

Classes, structs, interfaces and functions take type parameters (`class Stack<T>`,
`interface Box<T>`, `function f<T extends Comparable<T>>`). Every instantiation is compiled
separately (monomorphization): no boxing, and bounds resolve to direct calls. Bounds are
interfaces (an interface with only fields is satisfied by any type with its fields), not object
types. Classes, structs, interfaces and type aliases may give type parameters **defaults**
(`interface Iterator<T, E = never>`), used when a type leaves the argument out (`Iterator<i64>`
is `Iterator<i64, never>`) and when nothing infers it in a `new` (`new D(1)` of
`class D<T = i64>` is a `D<i64>`); functions and methods cannot. As in TypeScript, a default
may use only the parameters declared before it, and must not need its own declaration's
defaults again (``type parameter `T` has a circular default`` for `class S<T = S>`).

```ts
class Stack<T> {
  items: T[] = [];

  push(x: T) {
    this.items.push(x);
  }

  pop(): T | null {
    return this.items.pop();
  }
}

const s = new Stack<string>();
s.push("a");
s.push("b");
console.log(s.pop() ?? "empty", s.items.length);
```

## `extend`

An `extend` block adds methods, getters, setters and static methods to an existing type,
builtins included, at zero cost (the calls are direct):

- **Any type**: `extend string`, `extend i64`, `extend Array<string>`, classes, structs and
  unions (including discriminated unions:
  `extend Shape { area(): f64 { switch (this.kind) … } }`).
- **Generic and blanket** extensions: `extend<T> Array<T> { … }`,
  `extend<T extends Named> T { … }` (every implementor gets the method).
- **Static methods**: `extend Point { static origin(): Point { … } }` is called as
  `Point.origin()`.
- A type's own member wins over an extension. Among the extensions that apply (target matches,
  bounds hold), the most specific wins: block A is more specific than block B when A's target
  is an instance of B's and not the other way round, so `extend Array<string>` wins over
  `extend<T> Array<T[]>`, which wins over `extend<T> Array<T>` (the prelude's `join` on
  `string[][]` is its own). With the same target, a block with bounds
  (`extend<T extends Comparable<T>> Array<T>`) wins over one without. When no single block is
  more specific than every other one (`extend<T> Map<string, T>` and `extend<K> Map<K, i64>`
  on a `Map<string, i64>`, or two blocks with the same target that both have bounds or both
  have none), a call is an error: ``ambiguous extension method `m` ``, naming both blocks.
- `private` is not allowed in `extend`, and an extension cannot add fields (the layout is
  fixed).
- A type becomes `Comparable` by defining `compareTo` in an `extend` block
  ([Comparable](#comparable)), and `Iterable<T, E>` by defining `[Symbol.iterator]()`
  returning an `Iterator<T, E>`, or as a generator method (`*[Symbol.iterator](): Generator<T,
  E>`); `[Symbol.asyncIterator]()` makes it an `AsyncIterable<T, E>`
  ([Iterables](control-flow.md#iterables)). A generator method there that throws must write
  its error type in its result (`Generator<T, E>`): the interface's type is fixed before
  bodies are checked.
- Scope today: an extension applies wherever its module is loaded; `extend` blocks cannot be
  exported.
- **Planned** ([TypeScript alignment §4](../internals/design/ts-alignment.md#4-extend--full-power-zero-cost-module-scoped)):
  `static readonly` constants in `extend`; retroactive
  `extend Point implements Comparable<Point>`; module scoping (an extension is visible where it
  is imported) with ambiguity errors.

```ts
type Shape = { kind: "circle"; r: f64 } | { kind: "square"; side: f64 };

extend Shape {
  area(): f64 {
    switch (this.kind) {
      case "circle":
        return Math.PI * this.r * this.r;
      case "square":
        return this.side * this.side;
    }
  }
}

extend string {
  get initial(): string {
    return this.slice(0, 1).toUpperCase();
  }
}

extend<T> Array<T> {
  count(): i64 {
    return this.length as i64;
  }
}

class Point {
  x: f64 = 0.0;
  y: f64 = 0.0;
}

extend Point {
  static at(x: f64, y: f64): Point {
    const p = new Point();
    p.x = x;
    p.y = y;
    return p;
  }
}

const s: Shape = { kind: "square", side: 2.0 };
console.log(s.area(), "velt".initial, [1, 2, 3].count(), Point.at(1.0, 2.0).y);
```

## Comparable

The prelude declares `interface Comparable<T> { compareTo(other: T): i64; }` and implements it
for every number type, `string` (bytewise) and `bool`. With `T extends Comparable<T>`, the
operators `<`, `<=`, `>` and `>=` work on `T` (static dispatch after monomorphization), and
`sort()` orders Comparable elements (floats put `NaN` last). User types implement it with
`implements Comparable<X>` or an `extend` block, and then `<`, `<=`, `>` and `>=` work on their
values too (`v1 < v2` is `v1.compareTo(v2) < 0`).

```ts
class Version implements Comparable<Version> {
  major: i64;
  minor: i64;

  constructor(major: i64, minor: i64) {
    this.major = major;
    this.minor = minor;
  }

  compareTo(other: Version): i64 {
    return this.major != other.major ? this.major - other.major : this.minor - other.minor;
  }
}

function max<T extends Comparable<T>>(xs: T[]): T | null {
  let best = xs.pop();
  for (const x of xs) {
    if (best != null && x > best) {
      best = x;
    }
  }
  return best;
}

console.log(max([3, 9, 2]), max(["pear", "zoo"]), max([new Version(1, 2), new Version(1, 10)])?.minor);
```
