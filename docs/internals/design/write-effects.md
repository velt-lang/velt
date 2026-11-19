# Declared write effects

Status: partly implemented. Issue: [#854](https://github.com/velt-lang/velt/issues/854).

## Summary

An HTTP handler runs as a copy on each connection's task, and each request works on its own copy
of what the handler captured; a spawned task works on a copy of its arguments in the same way
([async reference](https://veltlang.com/docs/reference/async/)). A change made to such a copy is
lost, where Node, which shares the object, keeps it. Velt reports such a change at compile time,
naming the field, and suggests sharing the object.

This note describes the rule that decides what counts as such a change: every function has an
**effect summary** of what it writes and stores through its parameters, inferred from the body
for program and package code; std's are to be declared with checked doc-comment tags ([Not yet
implemented](#not-yet-implemented)). The analysis is static and free at run time, and keeps safe
TypeScript compiling.

## Motivation

```ts ignore
class C { count = 0; handle(): Response { this.count++; return new Response(`${this.count}`); } }
const c = new C();
await serve({ port: 0 }, async (req) => c.handle());   // Node: 1 2 3 …; a copy: 1 1 1 …
```

Code ported from TypeScript silently gives different answers unless the compiler sees the write.
The write is often far from the handler: in a method of a captured object, in a helper that
stores a captured list into a field of a new object through an `any`, in a plugin hook called
through a function value, or in a proxy's `set` trap. A rule that treats library calls as
opaque, or that guesses from types, either misses these or rejects safe code that only writes
objects the request made itself.

## Design

### Terms

- **A crossing** is a closure that a library runs on other threads (a server's handler), or a
  spawned call. Its **roots** are the closure's captures, or the spawned call's arguments and
  receiver.
- **Reached** describes a root, or a value read from one through fields, elements, entries,
  aliases, or calls that return part of it.
- **Holds** describes a new value (`new`, a literal, a spread, a fresh call result) that has a
  path to a reached value because something stored it there. A new value's own slots are the
  request's own, and writing them loses nothing.
- **Lost** describes a write to a slot of a reached value, directly or through a new value that
  holds it. This is what the crossing check reports, unless the slot is in a `shared` value, an
  `Atomic` or a `Mutex` (shared by every thread), or in a handle that the runtime shares.

### Summaries over access paths

Each function gets an effect summary whose relations are over **access paths**, so `ctx.out` and
a captured `app.plugins` stay apart even when their types are the same. What is built of it is
the summary of stores by key, `stores v in o.*` or `stores v in o.out` for a constant key, with
latent `calls` through function-typed parameters; the general relations over bounded paths below
are [not yet implemented](#not-yet-implemented).

A path is a **root** followed by at most *k* = 3 **selectors**. Roots are a parameter, `this`, a
closure's capture, and `ret` (the value the function returns). Selectors are `.f` (a field, keyed
by its declaring type and index, not by name), `[*]` (any element of an array, entry of a `Map`
or `Set`, or slot of a tuple) and `.*` (any field: a store or write by key). A longer path is cut
to its first *k* selectors with the suffix `.**`, anything below. `.*` covers every `.f` and
`.**` every suffix; a set of paths drops the paths another covers. Paths are bounded and a program
has finitely many fields, so the sets form a finite lattice and the fixpoint terminates.

A value at path π converted to `any`, `unknown`, `object` or a type parameter keeps π. A later
access through it with a constant key (`self.out`, `Reflect.set(o, "out", v)`) names the fields of
that name in π's static type and its subclasses; with a computed key it is `.*`.

For a function `f`, with π, σ and τ paths over `f`'s roots:

| relation | meaning | example |
|---|---|---|
| `writes π` | `f` may change the slot at the end of π | `writes this.count`, `writes c.out[*]`, `writes o.*` |
| `stores σ in τ` | the value at σ, with all it reaches, may be put in slot τ | `stores a in this.app`, `stores x in this[*]` |
| `returns σ` | the result may be the value at σ, or a new value described by `ret.…` stores | `users.get(id)` → `returns this[*]` |
| `calls φ(σ1, …)` | `f` calls the function values at φ; resolved at each caller | `calls f(this[*])` |

**Closures.** A closure made in a body contributes its writes and stores to its captures at the
point where it is made, as effects of the function that makes it. So
`function mk(a: App) { return () => a.plugins.push("x"); }` has `writes a.plugins[*]`, and
`const h = mk(app)` in a handler is reported although `h()` runs later.

**Applying a summary at a call.** Each root of the callee's summary is replaced by what the
argument is in the caller: a path from one of the caller's roots, a new value with the paths it
holds, or a plain value (numbers, booleans, and strings not passed to an extern that writes
them).

**Example.** A class that fills a field from a capture through an `any` is reported:

```ts ignore
class ThroughAny {
  app: App; out: string[] = [];
  constructor(a: App) { this.app = a; this.fill(); }
  fill() { const self: any = this; self.out = this.app.plugins; }
}
await serve({ port: 0 }, async (req) => {
  const c = new ThroughAny(app);
  c.out.push("x");   // error: c.out may be app.plugins
  return new Response("ok");
});
```

`fill` has `stores this.app.plugins in this.out`; composed in the constructor after
`this.app = a` it is `stores a.plugins in this.out`. So `c` holds `app.plugins` at `c.out`, and
`c.out.push("x")` writes a slot of a reached value. A context object that only holds the capture
(`new RenderCtx(app)` whose `out = []` is its own) and is written through its own fields is not
reported.

### Inference

Summaries are a fixpoint over the call graph, computed on demand for the functions a crossing
reaches by a worklist that re-applies only the events whose callee's summary grew, and cached for
one compile. The body rules (for `writes`, which are not summarized yet, the walk of `changes.rs`
follows the same calls):

- **Places.** A write to a place gives `writes` of its path; an assignment, or a literal or spread
  that ends up in a slot, gives `stores` of the value's paths into that slot.
- **Direct calls** apply the callee's summary. **Constructors** apply it with `this` as the new
  value.
- **Dispatched calls** (virtual, interface, bound-method) join the summaries of the methods they
  may run: the program's implementations, plus the functions an object type's literals store.
  An interface value obtained from `any` has no known implementations, so it counts as unknown
  ([Function values](#function-values)).
- **Intrinsics**: `AnySet` and `AnySetIndex` write and store into `target.*` (or the named field),
  `AnyDelete` writes `target.*`, array and collection operations write and store on their own
  slots.
- **Writes that run code are calls.** A write to an accessor runs its setter; a write to a value
  that may be a proxy runs the program's `set` traps, with the trap's `target` bound to the
  proxy's target. So a write through a reactive proxy over captured state (a `set` trap that
  calls `Reflect.set(target, prop, v)`) is reported.

### Function values

Only values from outside the program are unknown: a function or an interface value a host or a
native package's extern hands the program. Every other function value was made by program,
package or std code, and a call through it joins every function that could be there.

- **A function-typed parameter.** A call through one is the latent relation `calls f(σ1, …)`.
  Each caller resolves it against the argument it passes: a named function or closure (that
  function's summary), another function-typed parameter (lifted into the caller's summary), or a
  value read from the heap (below). So a generic `emit<T>(hooks: ((p: T) => void)[], p: T)` does
  not join every one-argument function in the program.
- **A function value read from the heap** (a field, an array element, a `Map` value) joins the
  summaries of every function value the program makes as that function type: closures and named
  functions used as values, method values, and functions stored in object-type literals. A
  function value converted to another function type is a new value made as that type, so a type
  matches only itself. A call through a capture or through a parameter of a function value (which
  no caller names) is joined the same way.
- **A function read from an `any`** (`const f = h.go; f(c, v)`, `(plugin as any)[hook]?.(ctx)`)
  joins, per list of argument types, the function values that accept them and the methods of the
  classes the program converts to `any` and of the classes those hold.
- **Unknown values.** When the program may receive a function from outside (a host extern, or a
  package's native function returning an `any`, a function or an interface value), a call that
  may run one is an error when it is given a reached value that is not `shared`, or a new value
  that holds a capture.

The join is per type: one closure anywhere that writes `c.app.n` through a `(c: Ctx) => void`
makes every heap call of that type given a reached `Ctx` an error.

## Compilation

The check is in `crates/velt_sema/src/ownership/local_async/`, and runs once mutation inference has
given each parameter its pass mode, only when a crossing captures something
(`changes.rs`, `check_crossing_changes`):

- `changes.rs` walks the crossing's body and the program bodies it calls for writes to reached
  values (including calls whose parameter mutation inference marked `BorrowMut`), following
  calls through function values via `targets.rs`.
- `fed.rs` finds the fields of new values that may hold a captured value: those some code stores
  a value that is not new in (a constructor's `this.app = app`, an assignment, a literal, an
  initializer, a spread or conversion).
- `keyed/` computes the store summaries (`stores v in o.*`, or `in o.out` for a constant key) for
  stores by key, with closures counted where they are made, latent calls through function-typed
  parameters resolved per caller, and the heap and `any` joins (`targets.rs` `value_targets`).
  Package bodies are followed like the program's; std bodies are not.
- `targets.rs` enumerates dispatched targets, the function values the program makes with the
  types they are made as, the types converted to `any`, and whether an extern may hand the
  program a function from outside (`from_outside`).
- `callbacks/` reports sync callbacks reached from a crossing that assign a variable they
  captured.

## Runtime cost

None: the rule is static. At compile time the scans run only when a crossing captures something;
summaries are computed once per function and only for functions a crossing reaches. std calls
cost a pass-mode lookup; no std body is walked in a user's compile.

## Diagnostics

The error names the closure, the field and how the change happens, and suggests sharing:

```text
error: this async closure changes `c.out`, which may be a value reached from captured `a1`:
       `self.out = …` in `ThroughAny.fill` stores one there by key, so it must stay on the task
       that created it
```

```text
error: this async closure changes `hits` of captured `c3` (`withCallback(…)` changes `c3.hits`),
       so it must stay on the task that created it
```

A note suggests sharing the object (`const c = shared(new Mutex(new C(…)))` changed with
`c.with(…)`) or keeping what changes in a `shared(...)` or an `Atomic`. The goldens are in
`tests/golden/lang/errors/http_server/` (for example `http_handler_store_by_key`,
`http_handler_fn_value_changes`, `http_handler_code_on_write`, `http_handler_package_store`), and
the shapes that must compile in `tests/golden/std/http_server/handler_*.velt`.

## Compatibility

TypeScript shares every object, so no valid TypeScript program loses a write in Node. Velt
accepts the programs whose handlers write only what the request made, and rejects, with a fix,
those that write captured state. Some per-copy state is a documented difference from Node: a
`RegExp` with the `g` or `y` flag used with `test` or `exec` (its `lastIndex`) and a
`TextDecoder`'s pending bytes stay per copy.

## Alternatives considered

**Read-only captures.** Sound, but they reject safe TypeScript that writes a new value built from
a capture.

**A run-time freeze of the copy.** It costs a check on every field write and turns compile errors
into run-time 500s.

**A whole-program analysis of which classes are converted to `any`, without summaries.** Each
variant either rejected a context object that only holds the capture or missed a write through a
store by key.

**Decorators for the effects** (`@writes("this")`). TypeScript does not allow decorators on
functions, interface members or `declare` functions, so std files would stop parsing as
TypeScript. Hints like `@inline` need a body, which externs and interface methods lack.
`Readonly<T>` parameters are shallow and cannot express a store or a call. Doc tags have precedent
in Closure Compiler's `@modifies {this}` and `@nosideeffects`.

**Every function from `any` unknown.** Every function inside an `any` was put there by program,
package or std code unless an extern brought it in, so joining the functions converted to `any`
is sound and keeps plugin dispatch such as `(plugin as any)[hook]?.(ctx)` compiling.

## Limitations

- **Native package code**: its externs' tags are trusted, like an `unsafe` block's promise.
- **Host objects**: an object from a JS host is not a copy, so it is out of scope, and so is a
  host extern that keeps a reference it was given.
- **Per-copy state** (the `READERS` list in `changes.rs`) is a documented difference from Node.
- **Precision of the heap join** is per function type, not per allocation site.

## Not yet implemented

The analysis infers store summaries (stores by key, function values, closures, the joins) and
finds writes with the walk of `changes.rs`. The parts below are accepted and not built;
`packages/http-server/README.md` "Known limits" lists the shapes that still change only the
request's copy without an error.

- **Write summaries over bounded paths.** Writes are found by `changes.rs`'s walk per
  (function, parameter, root type), which stops below 48 calls (`MAX_DEPTH`); `writes` relations
  over the paths of "Summaries over access paths" replace it and `fed.rs`'s field scan.
- **Doc tags** ([Syntax: doc tags](#syntax-doc-tags)). The parser keeps effect tags on std and
  package items (a change to the `ast.rs` contract), `velt fmt` and `velt doc` handle them, sema
  reads and verifies them, and malformed tags are errors. `scripts/std-lint` requires a reviewed declaration on every exported std
  function with a parameter able to hold a reference or a function, with an allow list that only
  shrinks. Library calls in unreviewed modules keep the pass-mode rule.
- **`@cache` replaces the compiler's `READERS` list** (`changes.rs`: `Headers.get`,
  `RegExp.test`, …), so what std's methods do is said in std.
- **std's stores are seen**: `Reflect.set(this, "out", a.plugins)` followed by a write is
  reported (`tests/golden/std/http_server/handler_known_gap_store_by_key.velt` records that it is
  not yet).
- **std calling the program** ([below](#std-calling-the-program)): `@calls`, stored listeners, and hooks run by std on a captured value
  (`JSON.stringify(app)` with a writing `toJSON`).
- **Runtime externs that write strings or arrays**: `StringBuilder.append` on a captured builder
  is reported.
- **Spawn** ([#2014](https://github.com/velt-lang/velt/issues/2014)): the rule [below](#spawn), with an
  error golden for its forms (a direct argument, an alias, two spawns, a closure, a conditional, a
  receiver through a vtable and an interface, a view, an array `push`); goldens that rely on the
  copy move to `shared` values.
- **Summaries cached per package version**, keyed by the package's source hash.

### std calling the program

std's summaries describe std's own code, but a std call may also run the program's code:

- **Function values passed to std, and functions std stores**, use the `calls` relation, declared
  by tag: `forEach(f)` has `@calls f(this[*])`; a class that keeps listeners has
  `@stores cb in this` on `on(cb)` and `@calls this.listeners[*](args)` on `emit(...args)`.
- **Hooks on the arguments**: the program's `toString`, `toJSON`, `valueOf`, `[Symbol.iterator]`,
  `compareTo`, getters and setters, and proxy traps. They are picked by the arguments' static
  types at the program's call site, so `console.log(ctx)` follows only the hooks of `ctx`'s type
  and the types it can hold, not every `toString` in the program.

### Externs

- **`declare function` (runtime).** A function with a string or array parameter needs a reviewed
  tag, even when the answer is "no effects": `velt_rt_strbuf_push_str(out: string, s)` and
  `velt_simd_u8x16_store(v, to: u8[], at)` change their argument in place. A `@writes buf` on a
  string parameter writes the place the string was read from, so `StringBuilder.append` on a
  captured builder is reported. Externs that take only scalars need no tag.
- **`declare system` and `declare host`**: a non-Copy parameter not written `readonly` is written.
  Host externs have no `stores`.
- **Native packages**: their externs' tags are trusted, since they cannot be checked.
- **Handles**: a `u64` handle names state the runtime shares, so nothing is lost.

### Syntax: doc tags

Effects in std, and optionally on a package's exports, are doc-comment tags:

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
```

- **`@writes p` and `@stores q in p`.** `p` and `q` are parameter names or `this`, optionally with
  a path (`this[*]`, `target.*`). A bare `p` means `p`'s own slots (its fields, elements or
  entries and the private representation that holds them), not what those slots hold. std never
  writes what a collection's elements hold except through program hooks, so a copy
  (`[...app.plugins]`, `new Map(entries)`) followed by `push` or `set` writes only the new value.
  On an `any`, `unknown`, `object` or type-parameter parameter a bare `p` means `p.*`. A rest
  parameter stands for each of its elements.
- **`@calls f(args)`**: `f` is a function-typed parameter or a path to stored function values.
- **No tags means no effects.** A constructor needs no `@writes this` (its `this` is new) and
  needs `@stores a in this` for each argument it keeps.
- **`@cache` on a private field** drops writes to that field from the summary (stores into it
  still count). It is for state each copy may rebuild: a lazily parsed list, a memo,
  `RegExp.lastIndex`, a `TextDecoder`'s pending bytes. It cannot be verified, so it is reviewed
  like an extern's tag.
- **Malformed tags are errors**: an unknown parameter name, `@write`, `@stores x` without `in`.
- An overload set shares the tags of its first signature. Tags go on functions, methods,
  constructors, accessors, abstract and interface methods, and `declare function` externs.
  `velt fmt` keeps them, and `velt doc` renders them as a **Changes:** line.

### Where summaries come from

| Code | Summary |
|---|---|
| std, exported | its tags, required and verified |
| std, not exported | inferred from the body |
| the program | inferred; users write no effects |
| a package | inferred, or its tags where it declares them (optional, verified in its own tests) |
| intrinsics | sema's table |
| `declare function` (runtime) | its tags, required for string and array parameters |
| `declare system` / `declare host` | its `readonly` parameters |
| native package code | its externs' tags, trusted |

**Checking declarations against bodies.** A function with tags is checked: its inferred summary,
without writes to `@cache` fields, must be covered by its tags, or it is an error at the function
(`` `Map.get` writes `this.order`, but its doc comment has no `@writes this` ``). std's
implementations of an interface method stay within its tags; the program's may exceed them and
are followed through the hook rule. std is verified by a dedicated gate job; a user's compile
trusts std's tags. A package's tags are verified when the package's own tests compile and trusted
in its dependents.

### Spawn

A spawned call is an error when its summary writes a slot of one of its arguments or its
receiver, directly or through a new value that holds it, and that argument is neither a fresh
value nor moved into the call (the call is its last use). This covers aliases
(`const h = { c }; spawn(work(c)); log(h.c.n)`), two spawns of the same value, and closures that
capture it, without analysing the code after the spawn. `spawn((async () => { c.n++; })())` uses
the same summaries: the closure's writes to its captures count where it is made.
