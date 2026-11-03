# Design: declared write effects

Status: proposed (issue #854). The owner's decisions of 2026-11-02 and 2026-11-03 are in
[Decisions](#decisions). It also gives `spawn` its rule (#2014). Nothing here is implemented
yet. The conservative check it replaces is `velt_sema::ownership::local_async`, from #2024.

## Problem

An HTTP handler runs as a copy on each connection's task, and each request works on its own
copy of what the handler captured. A spawned task works on a copy of its arguments in the same
way (https://veltlang.com/docs/reference/async/). A change made to such a copy is lost. In
Node the object is shared, so code ported from TypeScript silently gives different answers:

```ts ignore
class C { count = 0; handle(): Response { this.count++; return new Response(`${this.count}`); } }
const c = new C();
await serve({ port: 0 }, async (req) => c.handle());   // Node: 1 2 3 …; a copy: 1 1 1 …
```

#2024 makes the common shapes a compile error that names the field. It covers:

- a write to a captured object's own fields, and writes made by its methods and their callees;
- aliases, and the `find`, `for…of`, `?.` and `any` shapes;
- collection methods, decided by the pass mode that mutation inference gave the parameter;
- fields of new objects that an assignment, a literal or a spread fills from the capture.

It is not sound, and these shapes stay silent:

1. **Library bodies are not looked into.** A std or package method changes its argument when
   inference marked the parameter `BorrowMut`. The exceptions are a hardcoded list of "readers"
   in the compiler (`READERS` in `changes.rs`: `Headers.get`, `RegExp.test`, …), which write
   only a cache or a cursor. Neither source is a contract. A change to a std body can turn a
   user's compile error on or off, and the list is std knowledge hardcoded in Rust.
2. **Stores by key are not seen.** The acceptance tests on #854 are D6, E2, F2, G1 and G4
   (`tests/golden/std/http_server/handler_known_gap_store_by_key.vlt`). In each, a store
   through `any`, `Reflect.set`, a helper or a generic converter puts the captured
   `app.plugins` into a field of a new object, and the handler then pushes onto it. Rounds 4–6
   of #2024 tried a whole-program analysis of which classes are converted to `any`. Each round
   either rejected sigx's render-context shape or missed a write.
3. **Calls through function values are not reported.** Their callee is not known
   (`Callee::Indirect`).
4. **Deep call chains are cut.** A change found deeper than 48 calls (`MAX_DEPTH`) is not
   reported.
5. **Writes that run code are not seen.** A write to a proxy runs its `set` trap on the target,
   and a write to an accessor runs its setter. sigx's `reactive(app.state).count++` is such a
   write. Strings are treated as plain values (`reach::plain`), so an extern that appends to a
   string buffer in place is missed (`StringBuilder.append` on a captured builder).

The goal is a rule that is sound up to the limits listed at the end, static, free at run time,
and that keeps safe TypeScript compiling.

## Decisions

From the owner, 2026-11-02 (#854, #2014):

- **The complete rule is declared write effects.**
  - std functions declare what they write through their parameters and receiver, and the
    compiler checks the declarations against their bodies.
  - The #854 analysis uses the declarations instead of treating library calls as opaque.
- **Function values.** A call through an unknown function value that receives a captured object,
  or something reached from one, is an error unless the object is `shared`.
- **Rejected alternatives.**
  - Read-only captures: sound, but they reject safe TypeScript.
  - A run-time freeze of the copy: costs a check on every field write and turns compile errors
    into run-time 500s.
- **Spawn (#2014).** A spawned call that changes an object still used after the spawn is a
  compile error. It uses the same rule.

From the owner, 2026-11-03 (on this note's first review):

1. **"Unknown function value" is narrowed** to values from outside the program: native code,
   packages without bodies, and a function taken from `any`.
   - A call through one of the program's own function values joins every program function that
     could reach it by type. That is sound because the join is complete.
   - A call through a function-typed parameter is resolved at the caller against the actual
     argument where possible, so a generic `emit<T>(hooks, ctx)` doesn't join every one-argument
     function in the program.
2. **A new value holding a capture, passed to a truly unknown callee, is reported**, as #2024's
   round-8 rule already does. sigx is unaffected: its plugin hooks are program code, so they
   are followed.
3. **Packages may declare optional, checked tags** on their exports. Effects are inferred by
   default. A declared tag is checked against the body in CI, so an effect change in a
   dependency shows up as a diff in its API.
4. **`RegExp.lastIndex` and `TextDecoder` stay per copy**, documented in the async reference
   as a difference from Node.

## Terms

- **A crossing** is a closure that a library runs on other threads (a server's handler), or a
  spawned call. Its **roots** are the closure's captures, or the spawned call's arguments and
  receiver.
- **Reached** describes a root, or a value read from one through fields, elements, entries,
  aliases, or calls that return part of it. #2024 calls this `direct`.
- **Holds** describes a new value (`new`, a literal, a spread, a fresh call result) that has a
  path to a reached value because something stored it there. #2024 calls this
  `direct = false`. A new value's own slots are the request's own, and writing them loses
  nothing.
- **Lost** describes a write to a slot of a reached value, either directly or through a new
  value that holds it. This is what the crossing check reports, unless the slot is in a
  `shared` value, an `Atomic` or a `Mutex` (shared by every thread), or in a handle that rt
  shares (`reach::sharing`).

## The model: summaries over access paths

Each function gets an **effect summary**. Its relations are over **access paths** (as in
#2024's re-rooting of `this.count` as `c.count`), so `ctx.out` and the captured `app.plugins`
stay apart even when their types are the same.

### Paths

A path is a **root** followed by at most *k* **selectors**:

- **Roots:**
  - a parameter;
  - `this`;
  - a closure's capture;
  - `ret`, the value the function returns (a new object it builds, or a part of an argument).
- **Selectors:**
  - `.f`: a field. A field is keyed by its declaring type and index, as `fed.rs` keys it, not
    by name.
  - `[*]`: any element of an array, any entry or value of a `Map` or `Set`, or any slot of a
    tuple.
  - `.*`: any field, used for a store or a write by key.

**The bound.** *k* = 3 (`ctx.app.plugins[*]` has three selectors). A longer path is cut to its
first *k* selectors and gets the suffix `.**`, which means anything below that point.

**Coverage.** `.*` covers every `.f`, and `.**` covers every suffix. A set of paths is kept
normalized: a path that another path in the set covers is dropped. Paths are bounded, and a
program has finitely many fields, so the sets form a finite lattice ordered by coverage, and
the fixpoint terminates.

**`any` aliases.** A value at path π converted to `any`, `unknown`, `object` or a type
parameter keeps π. A later access through it:

- with a constant key (`self.out`, `Reflect.set(o, "out", v)`): the fields of that name in π's
  static type and its subclasses;
- with a computed key: `.*`.

### Relations

For a function `f`, where π, σ and τ are paths over `f`'s roots:

- **`writes π`.** `f` may change the slot at the end of π: `writes this.count`,
  `writes c.out[*]` (`c.out.push(…)`) or `writes o.*` (`o[k] = v`).
- **`stores σ in τ`.** The value at σ, with everything it reaches, may be put into the slot τ:
  `stores a in this.app`, `stores this.app.plugins in this.*`, `stores x in this[*]` (`push`).
- **`returns σ`.** The result may be the value at σ (`users.get(id)` → `returns this[*]`), or
  a new value described by the `ret.…` stores.
- **`calls φ(σ1, …)`.** `f` calls the function values at φ with arguments σi. φ is a
  function-typed parameter, or a path to function values stored in an object (`this.listeners[*]`).
  This relation is latent: it is resolved at each caller of `f` against what φ is there
  ([Function values](#function-values)).

**Closures.** A closure made in a body contributes its own writes and stores to its captures
at the point where it is made, as effects of the function that makes it. That is #2024's
`Event::Closure` rule, kept explicitly. `function mk(a: App) { return () => a.plugins.push("x"); }`
therefore has `writes a.plugins[*]`, so `const h = mk(app)` in a handler is reported even
though `h()` runs later. A closure value's summary over its parameters is used when it is
called. Its effects on its captures were already counted where it was made.

### Applying a summary at a call

At a call, each root of the callee's summary is replaced by what the argument is in the
caller. An argument can be:

- a path from one of the caller's roots;
- a new value from a site in the caller, with the paths it holds;
- a plain value: numbers, booleans, and strings that are not passed to an extern that writes
  them (see [Externs](#externs)).

Composition cuts paths at *k* the same way.

### Two walkthroughs

**`handler_ctx_to_callback` must compile.**

1. The handler captures `app`, which becomes root A. `render(app)` binds its parameter to A,
   so it is reached.
2. `new RenderCtx(app)`:
   - The constructor's summary is `stores app in this.app`. `out = []` stores a new literal,
     not a parameter.
   - So `ctx` is a new value holding A at `ctx.app`, and `ctx.out` is its own.
3. `ctx.emit(app.name)`:
   - `emit` has `writes this.out[*]`, so this call writes `ctx.out[*]`.
   - `ctx.out` is the request's own, so nothing is lost.
   - `app.name` is a string read, which is plain.
4. `ctx.out.push("!")` also writes `ctx.out[*]`, which is again the request's own.
5. `hooks[0](ctx)` calls a function value read from the heap. Its type is
   `(c: RenderCtx) => void`.
   - The join is over the program's function values of a matching type, and the only one is
     the closure, which has `writes c.out[*]`.
   - Applied to `ctx`, that is `writes ctx.out[*]`, the request's own.
   - Nothing from outside the program can have that type, so it is not unknown.
6. `ctx.app.plugins.length` is a read.
7. `setAny(o, k, v)` has `writes o.*` and `stores v in o.*`, but it is called only on
   `cfgAny` in `main`. That is outside the crossing, so it affects nothing here.

Nothing is reported.

**D6 (`ThroughAny`) must report.**

1. The constructor `constructor(a) { this.app = a; this.fill(); }`:
   - `this.app = a` gives `stores a in this.app`.
   - `fill()` contains `const self: any = this; self.out = this.app.plugins`. `self` aliases
     `this`, and the constant key `out` names `ThroughAny.out`, so `fill` has
     `writes this.out` and `stores this.app.plugins in this.out`.
   - Composing at `this.fill()` in the constructor, after `this.app = a`, gives
     `stores a.plugins in this.out`.
2. `new ThroughAny(app)` in the handler is a new value `c` that holds A at `c.app` and holds
   `A.plugins` at `c.out`.
3. `c.out.push("x")` writes `c.out[*]`, which is `A.plugins[*]`, a slot of a reached value.
   That write is lost.

Reported as:

```text
error: `c.out` may be `app.plugins`, which every request has its own copy of: ThroughAny's
       constructor stores it there (`self.out = this.app.plugins` in `fill`), and
       `c.out.push("x")` changes it
  help: make `app` `shared(...)`, or keep the list in a `Mutex`
```

E2 (`Reflect.set(this, "out", a.plugins)`) is the same once `Reflect.set` has its tags. F2 and
G1 are the same through `setKey`'s and `AnySetter.put`'s inferred summaries (`writes o.*`,
`stores v in o.*`). G4 resolves its function-typed parameter at the caller (see
[Function values](#function-values)).

## Inference

Summaries are a fixpoint over strongly connected components of the call graph, visited by a
worklist. They are monotone: path sets only grow, and the bound *k* makes them finite. Summaries
are computed lazily for the functions a crossing reaches, and cached per function for one
compile. This replaces #2024's walk, which re-walked a body for each
(function, parameter, direct, root type) key and stopped at `MAX_DEPTH`, and it replaces
`fed.rs`'s scan of fields.

The body rules:

- **Places.** A write to a place gives `writes` of its path. An assignment, or a literal or
  spread that ends up in a slot, gives `stores` of the value's paths into that slot.
- **Direct calls.** A direct call applies the callee's summary: a library callee by its tags,
  the program's by its summary.
- **Dispatched calls.** A virtual, interface or bound-method call joins the summaries of the
  methods it may run. Those are the program's implementations, plus the functions an object
  type's literals store, which `targets.rs` from #2024 enumerates today.
  - An interface value obtained from `any` (`raw as Plugin`) is unknown: the owner's rule
    applies.
- **Constructors.** `new C(args)` applies the constructor's summary with `this` as the new
  value.
- **Intrinsics.** A table in sema:
  - `AnySet` and `AnySetIndex`: `writes target.*` (or the named field) and
    `stores value in target.*`.
  - `AnyDelete`: `writes target.*`.
  - Array and collection operations: writes and stores on their own slots.
  - `new Proxy(t, h)`: `stores t in ret.target`, `stores h in ret.handler`.
- **Writes that run code.** These are calls:
  - A field write on a value whose type can hold a proxy (object types and views, `any`), and
    `AnySet` / `AnySetIndex`, join the program's `set` traps. The trap's `target` parameter is
    bound to the proxy's `.target`.
  - A write to an accessor runs its setter, and a `defineProperty` accessor installed on the
    value runs that function. Reads that run code (getters, `get` traps) are joined the same
    way.
  - `reactive(app.state).count++` therefore applies sigx's trap, whose
    `Reflect.set(target, prop, v)` writes `app.state.*`. That write is lost and reported.

## Function values

The owner's rule, narrowed on 2026-11-03: only values from outside the program are unknown.

**A function-typed parameter.** A call through one is the latent relation
`calls f(σ1, …)`. Each caller resolves it against the argument it passes:

- a named function or closure: that function's summary;
- another function-typed parameter: lifted into the caller's own summary;
- a value read from the heap: the heap join below.

This is how G4 is resolved: `viaCb(this, setK, "out", a.plugins)` resolves `f` to exactly
`setK`. A generic `emit<T>(hs: ((p: T) => void)[], p: T)` has `calls hs[*](p)`. Its caller's
`hooks` resolves through the heap join for that array's element type, not through every
one-argument function.

**A function value read from the heap** (a field, an array element, a `Map` value). The call
joins the summaries of every function value the program puts into a slot of a matching type:

- closures and named functions used as values;
- method values;
- functions stored in object-type literals. Their fields are found by `targets.rs`'s scan of
  literals, assignments to the field (including ones made later), and the sources of a spread.
  A literal that stores a parameter (`mkG(f)` gives `{ render: f }`) contributes whatever its
  callers pass as `f`.

A type matches when it has the same arity and parameter types that unify (type parameters
match anything). A conversion between function types carries its source's values, so a
`(x: any) => void` passed where `(c: Ctx) => void` is expected joins the values of the first
type, and carries the unknown taint if they have it.

**Unknown values.** These are a function from `any` (`AnyTo` to a function type, and
`AnyCall`), an interface from `any`, a function value returned or passed in by a native or host
extern, and one from a package without bodies. A call that may run one is checked as follows:

- **(a)** A reached value among its arguments is an error unless it is `shared`.
- **(b)** A new value that holds a capture is also an error (decision 2). "Holds" is decided by
  `stores` relations, never guessed from types, so `hooks[0](ctx)` with known hooks is not
  affected.

```text
error: `plugin.setup` may be a function from outside the program (taken from `any` at
       a.vlt:8:15), and it is given `ctx`, which holds `app`: every request has its own copy
  help: make `app` `shared(...)`, or keep the state it changes in an `Atomic` or a `Mutex`
```

**Precision.** The heap join is per type. One closure anywhere in the program that writes
`c.app.n` through a `(c: Ctx) => void` makes every heap call of that type with a reached `Ctx`
an error. The message names that closure's location. Following values per allocation site can
come later if the per-type join rejects real code.

## std calling the program

std's tags describe std's own code. A std call may also run the program's code. The summary
records this in two ways:

- **Function values passed to std, and functions std stores.** These use the `calls` relation,
  declared by tag. `forEach(f)` has `@calls f(this[*])`. A class that keeps listeners (std's or
  a package's) has `@stores cb in this` on `on(cb)` and `@calls this.listeners[*](args)` on
  `emit(...args)`. A call through stored listeners resolves through the heap join, restricted
  to the values stored by a `@stores … in this` of that class.
- **Hooks on the arguments.** These are the program's implementations of std interfaces and
  hook methods: `toString`, `toJSON`, `valueOf`, `[Symbol.iterator]`, `compareTo`, getters and
  setters, and proxy traps. They are picked by the arguments' **static types at the program's
  call site**, and the types those can hold, not by std's parameter type.
  - `console.log(ctx)`, `String(ctx)`, `JSON.stringify(ctx)` and `` `${ctx}` `` follow only
    `RenderCtx`'s and `App`'s hooks, not every `toString` in the program. That is the precision
    #2024 round 5 required.
  - Each hook is followed with its parameters standing for the arguments.

## Externs

- **`declare function` (rt).** A function with a parameter that is a string or an array needs a
  reviewed tag, even when the answer is "no effects".
  - `velt_rt_strbuf_push_str(out: string, s)` and its siblings, and
    `velt_simd_u8x16_store(v, to: u8[], at)`, change their argument in place.
  - A `@writes buf` on a string parameter writes the **place** the string was read from
    (`this.buf`, and so `this`). `StringBuilder.append` on a captured builder is then
    reported. Inference stops treating a string as plain where it reaches such a parameter.
  - Externs that take only scalars need no tag.
- **`declare system` and `declare host`.** These keep their existing rule: a non-Copy parameter
  not written `readonly` is written. Host externs have no `stores`, so a host that keeps a
  reference is out of scope.
- **Native packages.** Their externs' tags are trusted, since they cannot be checked. A function
  value passed to a native extern is followed with the extern's other arguments standing for
  its parameters, as under "std calling the program".
- **Handles.** A `u64` handle passed to rt names state that rt shares. It is not copied, so
  nothing is lost.

## Syntax: doc tags

Effects in std, and optionally on a package's exports, are doc-comment tags on the declaration:

```ts ignore
/**
 * Adds an entry, or replaces the value of an existing key.
 * @writes this
 * @stores key in this
 * @stores value in this
 */
set(key: K, value: V): this { … }

/** The value stored under `key`. */            // no tags: writes and stores nothing
get(key: K): V | undefined { … }

/**
 * Calls `f` with each element and its index.
 * @calls f(this[*])
 */
forEach(f: (x: T, i: i64) => void): void { … }

/**
 * Sets a property, as JavaScript's `Reflect.set` does.
 * @writes target
 * @stores value in target
 */
export function set(target: any, key: PropertyKey, value: any): bool { … }
```

- **`@writes p` and `@stores q in p`.** `p` and `q` are parameter names or `this`, optionally
  with a path (`this[*]`, `target.*`). A bare `p` means `p`'s **own slots**: its fields,
  elements or entries, together with the private representation the class allocates for them.
  It does not mean what those slots hold.
  - This is the split between a value's own slots and what they hold. std never writes what a
    collection's elements hold, except through program hooks, which `@calls` and the hook rule
    cover. So `[...app.plugins]`, `new Map(entries)` or a spread copy followed by `push` or `set`
    writes only the new value, as #2024's library arm decides today ("a new value … changed by
    the library is the request's").
  - On an `any`, `unknown`, `object` or type-parameter parameter, a bare `p` means `p.*` (by
    key).
  - A rest parameter stands for each of its elements.
- **`@calls f(args)`.** `f` is a function-typed parameter, or a path to stored function values.
- **No tags means no effects.** A function with no tags writes, stores and calls nothing. A
  constructor's own `this` is new, so it needs no `@writes this`. It needs
  `@stores a in this` for each argument it keeps (`new Map(entries)`).
- **`@cache` on a private field** drops **writes** to that field from the summary. A store into
  a cache field is still a store, or a memo holding a captured reference would be a hole. It is
  for state each copy may rebuild from its other fields: a lazily parsed list, a memo,
  `RegExp.lastIndex`, a `TextDecoder`'s pending bytes (decision 4). `@cache` cannot be verified,
  so it is reviewed like an extern's tag. It replaces the compiler's `READERS` list, so what
  std's methods do is said in std, not in the compiler.
- **Malformed tags are errors**, not ignored: an unknown parameter name, `@write`, or
  `@stores x` without `in`.
- **Overloads.** An overload set shares one set of tags, taken from the doc comment of its first
  signature.
- **Tools.** `velt fmt` keeps the tags. `velt doc` renders them as a **Changes:** line, as it
  renders `@throws`, and the editor shows them on hover.
- **Where the tags go.** Functions, methods, constructors, accessors, abstract and interface
  methods, and `declare function` externs.

Reasons for doc tags over the alternatives:

- **Decorators** (`@writes("this")`) would be the more typed syntax. TypeScript does not allow
  decorators on functions, on interface members or on `declare` functions. #1664's compile-time
  decorators are limited to classes, structs and their fields, so a std file would stop parsing
  as TypeScript.
- **Body markers** like `inline()` (std/lang/intrinsics/inline.vlt) need a body. Externs and
  interface methods have none.
- **`Readonly<T>` and `readonly T[]` parameters** are shallow, and they cannot express a store
  or a call.

Doc tags carry meaning already: the JSON design's `@decorator` tag is read by sema, and Closure
Compiler's `@modifies {this}` / `@nosideeffects` are the precedent from the JavaScript world.
Doc comments reach only `velt doc` today (`velt_doc::comment`). The parser keeps the effect
tags of an item in std or in a package as data on the item. That is a change to `ast.rs` and
needs a maintainer's sign-off, like any contract change.

## Where summaries come from

| Code | Summary |
|---|---|
| std, exported | its tags, required and verified |
| std, not exported | inferred from the body (only std bodies call it) |
| a program | inferred; users write no effects ("mutation is inferred") |
| a package | inferred, or its tags where it declares them (optional, verified; decision 3) |
| intrinsics | sema's table ([Inference](#inference)) |
| `declare function` (rt) | its tags, required for string and array parameters ([Externs](#externs)) |
| `declare system` / `declare host` | its `readonly` parameters, as today |
| native package code | its externs' tags, trusted |

**Required on std's exports.** `scripts/std-lint` requires a reviewed effect declaration on
every exported function and method that has a parameter able to hold a reference or a
function. "No tags" is a valid declaration, and it is verified. Until a module is reviewed, it
is on an allow list that only shrinks, like the std layout rule's list. An unreviewed module's
functions keep today's behaviour: pass modes, `READERS`, and bodies not looked into.

**Packages.** Tags are recommended on the exports of packages that take callbacks or hand out
hooks, sigx's among them. A later `velt publish` lint that reports an export whose inferred
effects grew would be cheap to add on top.

## Checking declarations against bodies

A function with tags is checked: its inferred summary, with writes to `@cache` fields dropped,
must be covered by its tags. Otherwise it is an error at the function:

```text
error: `Map.get` writes `this.order`, but its doc comment has no `@writes this`
```

Interface and abstract methods in std are handled as follows:

- **std's implementations.** Each must stay within the interface method's tags.
- **The program's implementations.** These may exceed the tags (rejecting them would reject
  safe TypeScript). Their own summaries are added where std may call them, through the hook rule
  under [std calling the program](#std-calling-the-program).

**When.**

- **std.** `cargo xtask std-effects` compiles every std module with verification on. `cargo
  xtask affected` selects it when `std/` or the analysis changes, so it runs in the gate and in
  the merge queue. A user's compile, and `velt check --std` (the nightly row
  `compile/check_all_std`), trust the tags and do not verify them.
- **A package's tags.** These are checked when the package's own tests compile (`velt test` in
  the package, which its CI runs), and trusted in its dependents' compiles.

## Reporting

The errors keep #2024's form. They name the field and the call chain, and suggest `shared(...)`,
an `Atomic` or a `Mutex`. The new forms are:

- **A write by key.** "`setKey(this, …)` writes `c` by key".
- **A store followed by a write.** This is the D6 message above.
- **A call that may run unknown code.** This is the message under
  [Function values](#function-values). It names where the value came from.
- **A write that runs a trap or a setter.** "`state.count++` runs the proxy's `set` trap
  (a.vlt:4:3), which writes `app.state`".

## Spawn (#2014)

**The rule.** A spawned call is an error when its summary **writes a slot of one of its
arguments or its receiver**, either directly or through a new value that holds it, and that
argument is **not a fresh value and not moved into the call**.

- **Moved** means the call is the argument's last use, as for `spawn(last.read(2))` in
  `spawn_dyn_receiver`.
- **Not "still used after".** This wording replaces "still used after the spawn" because it
  covers aliases (`const h = { c }; spawn(work(c)); log(h.c.n)`), two spawns of the same value,
  and closures that capture it, all without an alias analysis of the code after the spawn.
- **Closures.** The form `spawn((async () => { c.n++; })())` uses the same summaries. The async
  closure's writes to its captures are effects where it is made (see Closures), and its captures
  are copied into the task.
- **std's own spawns** are verified with std (`cargo xtask std-effects`) and skipped in a
  user's compile. Packages' spawns are checked.
- **The prototype.** The one in `local_async/boundary.rs`, removed in #2024, reused the #854 pass,
  and it switches to the summaries.

**The goldens that rely on the copy.** Their copying and leak coverage is kept, and the lost
writes move to a new error golden `tests/golden/lang/errors/spawn_writes_copy.vlt`. That golden
collects the forms: a direct argument, an alias, two spawns, a closure, a conditional
(`pick(i) ? bump(b) : bump(b)`), a receiver through a vtable and through an interface, a view,
and an array `push`.

| Golden | Change |
|---|---|
| `any_view_spawn` | `use(v)` stops writing `v` (it returns `v.price + 1`). `make(a, 7)` is given `a.clone()`. `make(new Product(), 8)` is fresh and stays. |
| `array_view_spawn` | the spawned `count(v)` stops pushing. The plain `await count(v)` keeps its `push`. |
| `share_async` | `total(xs)` stops pushing under `spawn`. The copy-then-push line moves to the error golden. |
| `spawn_dyn_receiver` | moves to `shared`: `n` becomes an `Atomic<i64>`, so the receivers (and their heap strings) are still deep-copied, and the totals match Node. |
| `spawn_promise_forms` | `Box.n` becomes an `Atomic<i64>` (`shared`). The conditional, method and stored-promise forms stay. |

These are contract files, so the step-4 pull request lists each change.

**Migration.** The async reference documents copy semantics for spawned tasks today. Step 4
changes behaviour that users can see: the reference is updated, the change gets a release-notes
entry, and it has its own docs-sync issue.

## Acceptance

**Must compile:**

- `handler_new_ctx_own_list`
- `handler_new_ctx_own_map`
- `handler_new_ctx_own_list_any_store`
- `handler_literal_own_buf`
- `handler_new_ctx_own_buffer`
- `handler_new_ctx_from_capture`
- `handler_new_page_from_capture`
- `handler_ctx_to_callback`
- `handler_ctx_to_iface`
- `handler_ctx_to_map_callback`
- `handler_ctx_to_iface_literal`

These are all in `tests/golden/std/http_server`.

**New goldens that must compile:**

- `handler_ctx_to_optional_hook`: `plugin.server?.setup?.(ctx)`, an optional function-valued
  property on a nested object. No golden has that shape today.
- `handler_ctx_to_log`: `console.log(ctx)`, `String(ctx)`, `JSON.stringify(ctx)` and
  `` `${ctx}` `` with a writing `toString` on an unrelated class.
- `handler_new_copy_then_push`: `[...app.plugins]`, `new Map(app.entries)` and a spread copy,
  each followed by `push` or `set`.
- `handler_emit_generic`: a generic `emit(hooks, ctx)` with a writing one-argument closure of
  an unrelated type elsewhere.

**Must report:**

- the 11 errors of `tests/golden/lang/errors/http_server/http_handler_own_field_fed`, including
  `fromAny`, `mkG`, `later` and the other-module `runner`;
- the 18 errors of `http_handler_changes_through_call`.

**Flips to an error golden:** `handler_known_gap_store_by_key`. D6, E2, F2, G1 and G4 each
report.

**New error goldens:**

- a hook run by std on a captured value (a writing `toJSON` under `JSON.stringify(app)`), and
  stored listeners run by `emit`;
- a closure returned by a reached function that writes its capture (`mk(app)`);
- `reactive(app.state).count++` through a proxy trap, and a write to a setter;
- `StringBuilder.append` on a captured builder;
- an interface from `any`, and a function from `any`, each given a new value holding a capture;
- `spawn_writes_copy`, listed under [Spawn](#spawn-2014).

## Limits

These stay outside the check, and are documented in the async reference:

- **Native package code.** Its externs' tags are trusted, like an `unsafe` block's promise.
- **Host objects.** An object from a JS host is not a copy, so it is out of scope. A host
  extern that keeps a reference it was given is too.
- **Handles.** rt handles (`u64`) name shared state, so nothing is lost.
- **Per-copy state.** `@cache` fields, including `RegExp.lastIndex` with `/g` and a
  `TextDecoder`'s pending bytes, are a documented difference from Node.

## Cost

**Today.** #2024's check does not walk library bodies: it reads pass modes and `READERS`. It
does walk the program's bodies again for each (function, parameter, direct, root type) key, up
to depth 48. It runs only when a crossing exists (`check_crossing_changes` returns early
otherwise).

**After this design:**

- **Run time.** None. The rule is static.
- **Program code.** Summaries are computed once per function, by an SCC worklist, and only for
  functions a crossing reaches. Paths bounded by *k* keep each summary small.
- **The scans.** The function-value and literal scans are whole-program. They run only when a
  crossing exists, as today's early return does.
- **std.** std calls cost a tag lookup, as today's pass-mode lookup does. No std body is walked
  in a user's compile.
- **The budget.** ±3% instructions retired on `hello` and `compile/check_all_std`
  (`bench/nightly.sh`), plus the http-server examples and sigx. Each step's pull request
  measures it in the Linux VM (cachegrind).
- **`cargo xtask std-effects`.** It runs in CI only. Step 2's pull request states its time, so
  the gate's budget is known.
- **Later.** Summaries can be cached per package version, keyed by the package's source hash.

## Order of work

Each step is a pull request:

1. **Summaries for program code.**
   - Paths, the four relations and the SCC fixpoint, replacing `MAX_DEPTH` and `fed.rs`.
   - Closures at the point where they are made.
   - Function values: the latent parameter calls, the heap join, and the unknown-value errors.
   - Proxy traps and setters.
   - The intrinsics table. Library calls keep today's pass-mode behaviour.
   - Acceptance: D6, F2, G1 and G4 report; the must-compile list compiles; the new goldens for
     closures, traps and `any` are added.
2. **Tags.**
   - The parser keeps effect tags on std and package items (an `ast.rs` change, with sign-off).
   - `velt doc` and `velt fmt` handle them, and malformed tags are errors.
   - Sema reads them, and `cargo xtask std-effects` verifies them.
   - `std-lint` gets the requirement with a full allow list.
   - `READERS` becomes `@cache`, and the rt externs that take strings or arrays get their tags.
   - Acceptance: E2 (`Reflect.set`) and `StringBuilder.append` report.
3. **Annotate std, module by module**, shrinking the allow list.
   - The collections, `net/http`, `reflect`, `encoding/json` and `fs` go first, since handlers
     use them most.
   - Library calls in annotated modules use their tags, and the pass-mode fallback goes with the
     last module.
   - Acceptance: the std-hook and listener goldens report; `handler_ctx_to_log` and
     `handler_new_copy_then_push` compile.
4. **Spawn (#2014)** on the summaries, with the goldens above, the async reference change, a
   release-notes entry and its docs-sync issue.
5. **Close the gaps.** Remove the "Known limits" entries in `packages/http-server/README.md`
   that the rule now covers. File the docs-sync issue for the HTTP handler rule in the async
   reference on `velt-lang/velt-lang.github.io`.

## Open questions

1. **`AnyCall` and functions from `any`.** Decision 1 counts a function taken from `any` as
   unknown. The review pointed out that every function inside an `any` in a compiled program was
   put there by program, package or std code; only host, wasm and native externs can bring one
   in. Treating a function from `any` as the join of the functions converted to `any` (and of
   methods with that name, for `AnyCall`) would be sound. It would also keep the common plugin
   dispatch `(plugin as any)[hook]?.(ctx)` compiling. Under decision 1 as written, that call is
   an error whenever `ctx` holds a capture. Keep decision 1, or take the join?
2. **The path bound.** Is *k* = 3 right? It is enough for every golden above. A larger *k*
   costs only compile time.
