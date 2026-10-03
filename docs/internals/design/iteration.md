# Design: iteration, generators and `for await`

Status: accepted (issue #62), implemented in four phases, **all built**: 1 (the protocol and
`for...of` over user iterables), 2 (sync generators), 3 (async generators and `for await`) and
4 (std sources are async iterables). Follow-up #424: builtin iterables and TS's iterator types
(section 6); consuming iterables, generator expressions and iterable object literals (section 7).

## Problem

TypeScript code iterates its own data structures (`for (const x of tree)`), streams lines and
messages (`for await (const line of lines)`), and writes lazy sequences with generators
(`function*`). Before this work, Velt's `for...of` only knew arrays, maps and classes with an
`entries()` method that builds an array, and there was no generator or async iteration at all.

## 1. The protocol (built, phase 1)

```ts ignore
type IteratorResult<T> = { value: T; done: false } | { done: true };

interface Iterator<T, E = never> {
  next(): IteratorResult<T> throws E;
  return(): IteratorResult<T> {           // early exit: release what the iterator holds
    return { done: true };
  }
}
interface Iterable<T, E = never> {
  [Symbol.iterator](): Iterator<T, E>;
}

interface AsyncIterator<T, E = never> {
  next(): Promise<IteratorResult<T>, E>;
  async return(): Promise<IteratorResult<T>> {
    return { done: true };
  }
}
interface AsyncIterable<T, E = never> {
  [Symbol.asyncIterator](): AsyncIterator<T, E>;
}
```

They live in `std/prelude/iter.vlt`.

- The done case has no `value` (Velt has no `undefined`), and there is no `TReturn`.
  `IteratorResult<T>` is an ordinary discriminated union on a `bool` literal field; `if (r.done)`
  and `if (!r.done)` narrow it (sema `body/narrow.rs`: a member access tested for truthiness on
  a union local whose discriminant values are all `bool` literals), and a union of `bool`
  literals is a condition (`body/expr/truthiness.rs`).
- `value` comes first, so a result prints and serializes as in Node (`{ value: 1, done: false
  }`). Reading `r.value` without narrowing is allowed on an `IteratorResult<T>` (recognized by
  its shape, `known.rs` `is_iterator_result`) and gives `T | null`, `null` when done: a match
  on the member (`body/expr/discriminated.rs` `union_field`). TS gives `undefined` there.
- Errors are typed: `E` is what `next()` throws. `for...of` rethrows it.
- **TypeScript's spellings** (sema `ts_protocol.rs`): TS's `Generator<T, TReturn, TNext>` (and
  `Iterator`, `Iterable`, the async twins) read the second argument as the return type. A second
  argument of `void`, `undefined`, `unknown` or `any`, and any third argument, are dropped
  (`Generator<number, void, unknown>` is `Generator<number>`; the parser accepts `undefined`
  there). Any other second argument is `E` and must be an error type (a class extending `Error`,
  a union of them, an interface, a type parameter); otherwise it can only be TS's `TReturn`, and
  is an error at the annotation. Checked once base classes are known.
- Not in the first version: `next(value)` (TS's `TNext`) and `throw()`. A later addition does
  not break code.

### What the protocol needed from the type system

- **Type parameter defaults** (`E = never`). Classes, structs, interfaces and type aliases take
  `T = Default` (parser `parse_type_generic_params`; sema `resolve.rs` `with_defaults` fills the
  missing arguments when a type is named, a default may mention earlier parameters). Functions,
  methods and arrows still reject defaults (TS uses them there only when inference fails).
- **A `throws` clause that names the interface's own type parameters.** Dispatch groups (sema
  `throws/groups.rs`) required one error type without type parameters per interface slot. Now a
  written clause on a generic interface's method may mention the interface's parameters: each
  member (implementation, default body, override) sees it with the interface arguments of its
  own `implements` (`member_bound`, walking `implements`, interface `extends` and base classes),
  and a call through an `Iterator<T, E>` value throws the clause substituted with the value's
  type arguments. This is sound because each instantiation of a generic interface has its own
  vtables: `Iterator<string, IoError>` and `Iterator<string, never>` entries never mix. HIR:
  `InterfaceMethodDef::throws` carries the slot's error type in the interface's parameters
  (contract change, hir_encodings.md "Errors"); VIR substitutes it at `Callee::Dyn` calls.
  An *inferred* group error type, and a base class method's clause, still cannot mention type
  parameters.
- **Inferring `E` from an implementation.** Matching `Iterable<i64, E>` against a class that
  implements `Iterable<i64>` binds `E = never`: interface arguments are matched like error types
  (`infer.rs` `match_iface_args`), where `never` binds a parameter.
- **Symbol method keys**: `[Symbol.iterator]` and `[Symbol.asyncIterator]` are method names like
  `[Symbol.dispose]` (parser `symbol_keys.rs`).

### Deviations from the accepted text

- `Iterator.return()` has a default body returning `{ done: true }` (`AsyncIterator.return()` an
  `async` one), so iterators that hold nothing need not write it; TS declares it optional
  (`return?()`). It returns `IteratorResult<T>` as in TS (first built returning `void`, which
  made the documented pattern throw a `TypeError` in Node); loops ignore the result.
- `[Symbol.iterator]()` of a class implementing `Iterable<T>` must be declared to return
  `Iterator<T, E>` (implementations match the interface signature exactly; Velt has no
  covariant returns). A class that does not declare `implements Iterable` may return its
  concrete iterator class, which `for...of` then calls directly.

## 2. `for...of` over an iterable (built, phase 1)

`for (const x of src)` keeps its array, `Map` and `entries()` loops unchanged. When `src`'s type
is not an array and has a `[Symbol.iterator]()` method (a class or struct, an `Iterable<T, E>`
value, or a bounded type parameter), sema (`body/for_iter.rs`) desugars the statement before
checking it:

```text
{
  let <iterator@N> = src[Symbol.iterator]();        // once; checked as HIR
  let <open@N> = false;
  try {
    label: while (true) {
      <open@N> = false;
      const <result@N> = <iterator@N>.next();       // throws E
      if (<result@N>.done) { break; }
      <open@N> = true;
      const pattern = <result@N>.value;             // `let` for `for (let ...)`
      { body }
    }
  } finally {
    if (<open@N>) { <iterator@N>.return(); }
  }
}
```

- `<open>` is true exactly while the body runs, so `return()` is called once when the body is
  left early (`break`, `return`, a thrown error, a labelled `break`/`continue` of an outer
  statement), and not on normal completion, `continue`, or an error thrown by `next()` itself,
  which is JS's behaviour (an iterator whose `next()` threw is finished).
- The `[Symbol.iterator]()` result must be an `Iterator<T, E>` (an interface value, or a class
  implementing it): else `` `[Symbol.iterator]()` must return an `Iterator<T>`, found `X` ``. An
  iterator is not iterable (TS requires `[Symbol.iterator]()` too); the "cannot iterate" error
  then says the value looks like an iterator.
- Everything after the first statement is checked like source code, so the existing rules apply
  unchanged: narrowing on `done`, typed errors (the `next()` call is an ordinary throwing call,
  so a function containing the loop throws `E`, inferred or checked against its `throws`
  clause), `try`/`finally` lowering, and ownership — the iterator is a local the block owns and
  drops at its end, and each `value` is moved out of the result into the binding.
- The hidden names cannot be written in source (`<`), and are unique per statement.
- A direct generator call (`for (const x of gen(args))`, or a generator `[Symbol.iterator]()`
  method) takes the embedded path of section 3 instead of the protocol.

**Cost.** One `[Symbol.iterator]()` call per loop; per element, one `next()` call (direct for a
concrete iterator class, through the vtable for an `Iterator<T>` value), a tag test on the
result (a small union value, no allocation), and two stores to a local flag. Leaving the loop
runs a `finally` block (no unwinding: errors are result returns). Iterating an
`Iterable<T>` *value* boxes the iterator it returns once per loop (an interface value owns a
heap box). Array and map loops are untouched.

**Diagnostics.**

| Code | Message |
|---|---|
| `for (const x of 5)` | `` cannot iterate over a value of type `i64` ``, noting what `for...of` accepts (arrays, `Map`s / `entries()` classes, `[Symbol.iterator]()`) |
| iterating an iterator | the same, plus "`Counter` looks like an iterator: iterate the iterable that creates it, or give it a `[Symbol.iterator]()` method" |
| `[Symbol.iterator](): Counter[]` | `` `[Symbol.iterator]()` must return an `Iterator<T>`, found `Counter[]` `` |
| `next()` returning `{ value: 1, done: false }` for `Iterator<string>` | the usual type mismatch at the literal |
| a loop over `Iterable<T, Read>` in a function `throws Other` | `` `f` throws `Read`, which its `throws` clause does not allow `` at the loop |
| `next()` throwing in `implements Iterator<i64>` | `` `C.next` throws `Read`, but `Iterator.next` does not allow it ``, noting "`E` is a type argument of `Iterator`: implement `Iterator` with `Read` as `E`" |

## 3. Generators (built, phase 2)

```ts
function* range(n: i64): Generator<i64> {
  for (let i = 0; i < n; i++) {
    yield i;
  }
}
```

- `function*` (and `*name()`, `static *name()`, `*[Symbol.iterator]()` methods) is a generator.
  Its declared result is `Generator<T, E>`, `Iterator<T, E>` or `Iterable<T, E>` (required); a
  written `E` is a `throws` clause, else `E` is inferred from the body like any `throws`, and a
  call has the declared type with that `E` (sema `collect/generator_sig.rs`,
  `body/generators.rs`; checked after inference like an async call's promise type). The call
  only creates the generator and never throws. `yield` is only allowed in generators (and not
  in an arrow inside one); `await` is not allowed in them; `return value` is an error (no
  `TReturn`); a bare `yield` needs `Generator<void>`.
- `Generator<T, E>` is a prelude **class** (`std/prelude/iter.vlt`) implementing
  `Iterator<T, E>` and `Iterable<T, E>`: `next()`, `return()`, `[Symbol.iterator]()` (returns
  `this` as an `Iterator<T, E>`) and `[Symbol.dispose]()` (closes it, so `using g = gen()`
  works and dropping it closes it). Its methods use the std-only intrinsics
  `__intrinsic_generator_resume` (throws `E`), `_value` and `_return`. `new Generator` and
  `extends Generator` are errors.
- `yield* src` is `for (const <yield@N> of src) { yield <yield@N>; }`: any iterable, and a
  direct generator call takes the embedded path below. Closing the outer generator while it is
  inside the loop leaves the loop early, which closes the inner iterator (`return()`, or the
  embedded state's close).

### Lowering (velt_vir `async_fn/generator.rs`, `async_fn/gen_object.rs`)

- A generator instance compiles like an async function: its poll function, built by the same
  code (`poll_fn` with `AsyncCx::generator`), is the **resume** function `f$poll(state, null)`
  — the `cx` parameter is kept so the liveness, spilling, frame-slot promotion (velt_opt
  `frame_slots`) and drop machinery apply unchanged — returning `0` DONE, `1` YIELDED (the value
  in the result region at offset 0: the `Ok` payload of `Result<T, E>` when the body throws,
  else `T`) or `2` THREW (the region holds `Err(e)`). Each `yield` stores the value, sets the
  tag to its suspension `k` and returns 1.
- **Closing** is the state's `$drop` (`Work::AsyncDrop`, unchanged: it sets `DROP_BIT` and calls
  the resume function). For a generator the dispatch case `DROP_BIT | k` of a `yield` runs what
  `return;` at that `yield` runs — every scope's drops *and `finally` blocks* — where async
  cancellation runs drops only. Before the first resume it drops the arguments; after the end
  it does nothing. Both leave the tag DONE, so `next()` after `return()` is done.
- Because closing runs `finally` blocks where the generator cannot pause, report an error or
  go on with its body, sema rejects `yield` in a `finally` block, a `finally` block that may
  throw, and a `break` / `continue` in a `finally` block that targets a loop outside it
  (`FnCx::loop_target`, `Frame::finally_loops`): lowered in the close path, such a jump would
  resume the body instead of finishing.
- While the body runs the tag is RUNNING; resuming or closing it then (the body reaching its
  own generator: `GEN_RUNNING`, `DROP_BIT | GEN_RUNNING`) panics with `generator is already
  running` (JS throws a `TypeError`). The store is dead in an inlined loop and disappears. The
  body cannot free its own state by dropping the last reference to its generator: a call's
  receiver reached through a counted object or a shared cell (a variable a closure assigns)
  is shared for the statement (velt_vir stabilize.rs), so the drop happens after the step.
- A **generator object** is one heap block `[table: ptr][state]` (the class's first field is the
  table pointer; states are at most 8-aligned; an async generator has its call queue between
  them, section 4): the static per-instance table holds resume,
  close and free functions. `Work::GenNew` builds it (counted like any object of the class when
  `Generator<T, E>` is shared), `Work::Fn` returns it or, for a declared `Iterator` / `Iterable`
  result, its interface value; the class's drop runs `[Symbol.dispose]` (close), then frees the
  block through the table (`object_free`). `console.log(g)` prints `Object [Generator] {}`
  like Node. A generator's state cannot be copied, so sema rejects every deep copy of a value
  holding one (`known.rs` `holds_generator`, `body/generators.rs` `no_generator_copy`):
  `clone()`, an argument (or receiver) of a spawned call, a value sent on a std `Channel`, and
  a capture of an async closure (whose state copies its captures when it runs), each with
  "a generator cannot be copied" / "... cannot be passed to another task" / "an async closure
  cannot capture the generator `g`". A generator coerced to an interface value at the spawned
  call itself (`spawn(sum(gen()))` with `sum(it: Iterable<T>)`) is caught too (the argument's
  `ToDyn` operand). An interface value made earlier is not looked into: one backed by a
  generator that reaches a spawned task still panics at run time (`a generator cannot be
  copied`, the clone glue). Moving a fresh generator into the task instead was not done: the
  task transfer deep-copies interface values without knowing whether they are shared.
- **Embedded loops**: `for (const x of gen(a))` with a direct call (sema emits `GeneratorEmbed`,
  hir_encodings.md "Generators") builds the state in a frame local (spilled into the enclosing
  state when the loop is in an async function or a generator, like an embedded awaited child),
  calls the resume function directly each step and reads the value slot; the local's drop
  closes it. A generator iterating a direct call of itself (recursion, `yield* walk(t.left)`)
  cannot embed its own state, so that call becomes a generator object.

### Cost

`bench/iter` (`bench/iter/run.sh`): 20 × 30M values summed modulo a prime, LLVM release, best
of 7 interleaved runs (Apple M4, shared with other builds: identical programs vary by up to
±10% between rounds):

| program | ms | vs hand loop |
|---|---|---|
| hand_loop (while loop) | 1172 | 1.00 |
| gen_loop (`for...of` over `range(n)`) | 1233 | 1.05 |
| iterable_class (iterator class) | 1147 | 0.98 |
| gen_value (`Iterable<i64>` parameter) | 2997 | 2.56 |

The embedded loop compiles to the hand loop's instructions (the same seven per value, rotated):
the resume function is inlined, the state is promoted to registers and the dispatch switch is
jump-threaded away; the difference in the table is noise. A loop the vectorizer can
handle as a hand loop (a filter into a plain sum) stays scalar as a generator, since the `yield`
is an exit from the producer's loop. A generator value costs an interface call and a table call
per step.

### Deviations from the accepted text

- `Generator<T, E>` is a class, not an interface: its `[Symbol.iterator]()` returns
  `Iterator<T, E>` (Velt has no covariant returns), and a generator value is the class's object.
- `resume` is the async poll function with a null `cx` (not a separate signature), and THREW is a
  third result.
- A `finally` block in a generator cannot `yield`, throw, or `break` / `continue` out of
  itself (TS allows all three; see above).
- `for...of` over a generator *value* goes through the protocol (`[Symbol.iterator]()`, then
  `next()` through the interface); only direct calls are embedded.

## 4. Async generators and `for await` (built, phase 3)

```ts
async function* lines(texts: string[]): AsyncGenerator<string> {
  for (const t of texts) {
    await sleep(1);
    yield t;
  }
}

async function main() {
  for await (const line of lines(["a", "b"])) {
    console.log(line);
  }
}
```

### Language

- `async function*`, `async *name()`, `static async *name()` and `async *[Symbol.asyncIterator]()`
  are async generators. The declared result is `AsyncGenerator<T, E>`, `AsyncIterator<T, E>` or
  `AsyncIterable<T, E>` (a sync result on an `async function*`, or an async one on a
  `function*`, is an error naming the fix); `E` is inferred from the body (awaited calls
  included) or written, like a generator's (`collect/generator_sig.rs`). The body may `await`
  and `yield`; `FnInfo::is_async_gen` is set and `FnInfo::is_async` is not (a call creates the
  generator, not a promise), and HIR `FnDef` has both `is_generator` and `is_async`.
- `AsyncGenerator<T, E>` is a prelude class like `Generator`: `async next()`, `async return()`,
  `[Symbol.asyncIterator]()` (itself), `async [Symbol.asyncDispose]()` (the awaited close) and
  `[Symbol.dispose]()` (the close without awaiting, also its drop), over the std-only
  intrinsics `AsyncGeneratorResume` / `Value` / `Return` / `Dispose` (hir_encodings.md "Async
  generators"). `new AsyncGenerator` and `extends AsyncGenerator` are errors.
- `for await (const x of src)` (`ast::StmtKind::ForOf::is_await`, sema `body/for_await.rs`) is
  only allowed in async functions and async generators ("`for await` is only allowed inside
  async functions" / "... is not allowed in a generator", naming the fix). Over a type with
  `[Symbol.asyncIterator]()` it is section 2's protocol loop with `await <it>.next()` and, on
  early exit, `await <it>.return()` (once, as JS). Over a direct async generator call it is the
  embedded loop below. Over a sync source (an array, an iterable, a generator) it is
  `for...of` with each value awaited when the element type is a promise, as JS does; an array
  of promises is consumed (its promises move out, so a variable holding it is moved). A sync
  `for...of` over an async iterable says to use `for await`.
- `yield* src` in an async generator is `for await (const v of src) yield v;`: async
  iterables, and sync ones as in JS (async-from-sync).
- `yield p` in an async generator, `p: Promise<T, E2>`, is `yield (await p)`, as JS does (sema
  `expr/tasks.rs` `yielded_awaiting`): the rejection is thrown at the `yield` and `E2` joins
  the generator's error type. No new HIR: an `Await` inside the `Yield` intrinsic's argument.

### Lowering (velt_vir `async_fn/generator.rs`)

- An async generator instance is an async state machine with both suspension kinds: its poll
  function `f$poll(state, cx)` returns `0` PENDING (an `await` suspended; the waker is
  registered as for any poll), `1` DONE, `2` YIELDED (value in the result region) or `3` THREW
  — the sync generator codes plus one (`FnLower::gen_code`). A finished state polled again
  returns DONE (an explicit dispatch case for `DONE`).
- `await AsyncGeneratorResume(g)` in the consumer is a suspension of the consumer: its resume
  block polls `g` with the consumer's own `cx` (through the object's table, or directly for an
  embedded state); PENDING suspends the consumer, THREW routes the error like a throwing call.
- **Closing.** Tag bit `CLOSE_BIT` (`0x4000_0000`; `DONE | CLOSE_BIT == DONE`) asks for the
  *awaited* close: `Work::AsyncCloseStart` (`f$close`, in the object's table) sets it, and the
  closer polls until not PENDING. The case `CLOSE_BIT | k` of a `yield` runs what `return;` at
  the `yield` runs, including `finally` blocks that `await` (their awaits are ordinary
  suspensions of the close path). The *dropping* close (`DROP_BIT`, `f$drop`, which runs with no
  `cx`) is the same block when that cleanup has no `await`; otherwise it is a cancel block (drops
  only, like a cancelled async function). At an `await` (a `next()` whose promise was dropped)
  both bits cancel the pending child. Cancel blocks of an async generator leave the tag DONE.
- **Embedded loops.** `for await (const x of agen(a))` keeps the state in a hidden local of
  the enclosing state machine (spilled into its state, like an awaited child) and is wrapped in
  `try { … } finally { await AsyncGeneratorReturn(<generator>) }`, so leaving the loop early
  awaits the close (a no-op once the generator is done: one tag test on the normal path).
  Cancelling the enclosing task drops the local, which runs the dropping close.
- **Heap objects** are the sync layout with the call queue's three words before the state
  (`[table][queue][state]`, "Overlapping calls" below); the table has a fourth entry
  (close-start). `await g.next()` on an `AsyncGenerator<T>` variable is a direct call of the
  class's async method, whose state the caller embeds (no allocation per item); through an
  `AsyncIterator<T>` interface value each `next()` is a boxed promise (one allocation per item).

### Async methods on resources

`await g.next()` passes `g` as the async method's (owned) receiver. Before this phase a
receiver or argument that owns a resource (`[Symbol.dispose]`) was always *moved* into the
call, so `using r = res(); await r.next()` was "cannot move `r` out of its `using`
declaration", and a last use disposed the object when the call finished. Now an object
receiver or argument of an async call is a soft move like any other object (`tasks.rs`
`note_async_args`, `method_call.rs` `receiver`), shared when the place is used again or is a
`using` variable (`ownership/validate.rs`); values holding a promise still move.

A `using` variable is disposed at the end of its block whoever else holds it (its drop), so
sharing one with a promise that outlives the block would run the call on a disposed object, or
(as the drop of the last reference) delay the disposal past the block. Sema therefore shares a
`using` variable only with a call awaited where it is made (`FnCx::soft_move` defers it,
`awaited_using_shares` accepts it when the enclosing `await`'s operand is that call); a stored or
returned call is "`r` is declared with `using`: an async call that keeps it must be awaited
here" (`finish_using_shares`), and `spawn(r.read())` / `spawn(use(r))` stay "cannot move `r`
out of its `using` declaration" (the task's deep copy would be disposed a second time). An
`await using` variable may be shared with a stored promise: its block awaits
`[Symbol.asyncDispose]()` explicitly at the end, as JS does.

### Overlapping calls on a generator object

`AsyncGenerator.next()` and `return()` follow JS's queue: a call made while another is running
waits for its turn, so each promise settles with its own step in call order, and a call whose
promise is dropped or abandoned (a started promise runs to completion) still takes its turn.
Without it, two `next()` promises polled the same suspended `await` with different wakers: the
values went to the wrong caller, a lost wakeup could hang the program, and `return()` during a
pending `next()` skipped `finally` blocks.

The queue lives in the prelude class (std/prelude/iter.vlt): three `u64` fields after the
table pointer — whether a call is running, `head` and `tail`, runtime latches
(`velt_rt_latch_*`, the ones `new Promise` uses). A call that finds none running runs at once
and allocates nothing. A call that has to wait creates the latch it will open when it finishes
and leaves it in `tail`, and waits on the latch of the call before it: the previous `tail`, or,
when only a call that started alone is ahead, `head`, which it creates for that call. A
finished call opens and releases its latch (`head` for one that started alone), which wakes
only the next call in line, or, when nobody queued behind it, clears `running`. So n
overlapping calls cost n wake-ups (a single shared latch woke every waiter on each turn:
O(n²)). `next()` and `return()` pass the turn on in a `finally`. All callers of one generator
run on its task (a generator cannot cross tasks), so the fields need no atomics, and a pending
call keeps the object alive, so none is queued when it is dropped. The generator object's state
now follows the class's fields (`Cx::gen_state_off`: the class object's size; 8 for
`Generator`, 32 for `AsyncGenerator`), and `Work::GenNew` zeroes the fields after the table
pointer. The embedded `for await` loop over a direct call has no object and no queue. A call
cancelled while it waits for its turn (only when its whole task is dropped) never opens its
latch; everything else on that task is dropped with it.

### Cost

`bench/iter` (`bench/iter/run.sh 7`): 20 × 3M values, each from an async call that completes at
once (`await step(i)`), summed modulo a prime; LLVM release, best of 7 interleaved runs (Apple
M4, shared with other builds):

| program | ms | vs async_hand | Node (ms) |
|---|---|---|---|
| async_hand (while loop, `await step(i)`) | 49 | 1.00 | 1876 |
| async_gen (`for await` over `values(n)`) | 118 | 2.41 | 7043 |

Both are far below Node, and neither allocates per value. The hand loop inlines `step` and
keeps everything in registers (about 0.8 ns per value); the generator loop costs about 1.2 ns
more per value: the generator's state lives in the caller's state *memory* (an async
function's state is not promoted to registers across its suspension points), so each step
loads and stores the generator's tag and counter, polls it and tests the result codes. A body
doing real work (an actual suspension, I/O) hides this; making the embedded state
register-resident across steps is a possible follow-up.

### Deviations from the accepted text

- The poll function has a fourth result (THREW) and the codes are shifted by one so that 0
  stays PENDING.
- `finally` blocks of an async generator may `await` (they run when it is closed with an
  awaited `return()`); a generator dropped without `return()` cancels such cleanup instead of
  running it (JS never runs it at all).
- An async generator cannot be passed to another task (sema rejects it, as for sync
  generators), rather than following the promise transfer rules: its state may hold objects the
  creating task shares.
- `for await` over a sync source awaits only promise elements; it adds no extra suspension per
  element, so a loop over plain values does not yield to other work as JS's would.

## 5. Std sources (built, phase 4)

Each std pull source is also an async iterable; the pull methods stay. The iterator is an async
generator method, so a direct `for await` over the source embeds the generator's state in the
caller (section 4): no allocation per item, the same runtime calls as the pull loop.

| Source | How | Yields | `E` |
|---|---|---|---|
| `Channel<T>` (std/channel.vlt) | `implements AsyncIterable<T>`, `async *[Symbol.asyncIterator]()` | values until closed and drained | `never` |
| `FileReader` (std/fs_stream.vlt) | `async *lines(): AsyncGenerator<string, IoError>` | remaining lines | `IoError` |
| velt:stdin | `export async function* lines()` | remaining lines | `IoError` |
| `WebSocket` (std/websocket.vlt) | `implements AsyncIterable<WsMessage, IoError>` | messages until the peer closed | `IoError` |
| `RedisSubscriber` (std/redis/pubsub.vlt) | `implements AsyncIterable<RedisMessage, RedisError>` | messages until `close()` | `RedisError` |
| `Ticker` (std/timers.vlt) | `implements AsyncIterable<i64>` | tick numbers 1, 2, … per loop, until stopped | `never` |
| postgres `CopyReader` | `implements AsyncIterable<string, PgError>` | `read()` chunks to the end | `PgError` |

`FileReader` and stdin get a `lines()` method rather than being iterable themselves: a reader
also reads bytes and chunks, so the line view is named, like Node's `filehandle.readLines()`.

**Early exit does not close the source.** JS stream iterators destroy the stream in `return()`;
here leaving the loop only closes the generator, which holds nothing: the channel, socket,
subscription or reader stays open where it was (the next value is still receivable). A channel
has other receivers and these handles are shared copies, so ending one consumer's loop must not
end everyone's stream; closing stays an explicit `close()` / `stop()`. The generators are
suspended at a `yield` whenever the loop body runs, so no received value is lost by a `break`.

**Generator methods in dispatch groups** (sema `throws/groups.rs`, `collect/impls.rs`). A
generator method implementing `[Symbol.asyncIterator](): AsyncIterator<T, E>` with `E` other
than `never` did not work before this phase: its signature keeps the result with `E = never`
and a written `E` as `throws` (section 3), so it did not match the interface method, and its
`E` joined the slot's dispatch group, merging every implementation's error type (`IoError |
PgError | RedisError`). Now the interface check compares the generator's result with its written
`E` restored (a missing one is reported with "declare the generator's result as
`AsyncIterator<T, E>`"), and generator methods are not members of dispatch groups: calling one
never throws, its `E` lives in the result type, so it neither adds to nor takes the group's
error type. This is sema-internal; HIR is unchanged.

### Deviations from the accepted text

- Leaving a loop early does not close the source (JS stream iterators destroy the stream).
- Beyond the four planned sources, stdin `lines()` and postgres `CopyReader` are iterable too.
- Generator methods leave dispatch groups (above), a sema change the plan did not foresee.

### Cost

`for await (const v of ch)` compiles to the generator's poll embedded in the loop, calling the
same `velt_rt_chan_receive` as `await ch.receive()`, with no allocation per value (checked in
VIR). Same build, LLVM release, `VELT_THREADS=1`, best of 21 interleaved (bench/RESULTS.md):
bench/async `channel_pipeline` (1M values from 4 producers) 44.6 ms with the `receive()` loop,
46.8 ms with `for await` (about 2 ns per value: the embedded generator step of section 4);
draining 10 × 1M queued values in one task 432.6 vs 428.6 ms. Every bench/async and bench/iter
program compiles to a byte-identical object file before and after this phase.

## 6. Builtin iterables and TypeScript's iterator types (#424)

Arrays, strings, `Map`s and `Set`s are `Iterable<T>`, so code written against `Iterable<T>`
takes them, and TS's `IterableIterator<T>`, `IteratorObject<T>`, `AsyncIterableIterator<T>` and
two-argument `IteratorResult<T, TReturn>` exist.

### Mechanism: `extend` blocks implement `Iterable`

`extend` blocks cannot list `implements` (retroactive `implements` is planned, classes.md
"extend"), so, as `compareTo` makes a type `Comparable` (`collect/comparable.rs`), an `extend`
block defining `[Symbol.iterator](): Iterator<T, E>` makes its target implement `Iterable<T, E>`,
and one defining `[Symbol.asyncIterator](): AsyncIterator<T, E>` makes it an `AsyncIterable<T,
E>` (`collect/iterable.rs`: an entry in `Program::impls`, generic when the block is). The rule
applies to user blocks too. The prelude (std/prelude/iter.vlt) has the blocks:

| Type | `[Symbol.iterator]()` returns | Iterates |
|---|---|---|
| `T[]` | `new ArrayIterator<T>(this)` | a live view: the iterator holds the array (a share, not a copy) and reads the length at each step; once done it stays done (JS) |
| `string` | `new StringIterator(this)` | characters (code points) as strings, by byte offset (`charAt`) |
| `Map<K, V>` | `new ArrayIterator(this.entries())` | the entries as of the call |
| `Set<T>` (std/collections/set.vlt) | `new ArrayIterator(this.values())` | the elements as of the call (a class method: `Set` declares `implements Iterable<T>`) |

So `[1, 2, 3]` converts to an `Iterable<i64>` value (`ToDyn` with the impl; the array's header
is boxed unless the array is already counted, and no element is copied), satisfies an
`Iterable<T>` bound (`ParamMethod` resolves to the extension), and `a[Symbol.iterator]()` is an
ordinary extension call returning an `Iterator<T>`. No HIR, VIR or runtime change was needed.
An array literal written where an `Iterable<T>` is expected takes `T` as its element type
(`sum([1, 2])` with `sum(xs: Iterable<f64>)`; `known.rs` `iterable_elem`).

**`for...of` keeps its loops.** `body/for_iter.rs` `is_iterable` already skipped arrays; it now
also skips `string` and the prelude `Map`, so `for (const x of xs)` over them is the same
`ForOf` as before and only code going through `Iterable<T>` (a value or a bound) uses the
protocol. Every bench/ program lowers to the same LLVM IR as before this change (`velt build
--release --emit llvm`, all 62 that compile), except one global's numeric symbol suffix in
bench/async `all_small_stored` (prelude items shift def numbering).

**The live array view costs a count.** `ArrayIterator` shares the array, so an array type
whose `[Symbol.iterator]()` is instantiated becomes counted program-wide (semantics stage 2:
boxed `{ data, len, cap }`), like any shared array; programs that don't iterate arrays through
`Iterable<T>` keep their representation.

### TypeScript's iterator types

```ts ignore
interface IterableIterator<T, E = never> extends Iterator<T, E>, Iterable<T, E> {}
interface IteratorObject<T, E = never> extends Iterator<T, E>, Iterable<T, E> {}
interface AsyncIterableIterator<T, E = never> extends AsyncIterator<T, E>, AsyncIterable<T, E> {}
```

- `Generator` implements the first two, `AsyncGenerator` the third; generators may be declared
  to return them (`known.rs` `SYNC_RESULTS` / `ASYNC_RESULTS`), and `ArrayIterator` /
  `StringIterator` implement the first two.
- Their second argument follows `Generator`'s TS rules (`ts_protocol.rs`, and the parser's
  `undefined` exception): `void` / `undefined` / `unknown` / `any` dropped, an error type is `E`,
  anything else is TS's `TReturn` and an error.
- `IteratorResult<T, TReturn>`: a "nothing" `TReturn` is dropped; any other one is an error
  (``` `IteratorResult` takes no return type: `string` is TypeScript's `TReturn` ```), since a
  finished result carries no value.
- **Interface values don't convert to the interfaces they extend** (a general Velt limitation:
  `ToDyn` needs the concrete type's impl). The prelude closes the gap that matters for
  iteration with blocks on the interface types themselves: `extend<T, E> IterableIterator<T, E>
  { [Symbol.iterator](): Iterator<T, E> { return this[Symbol.iterator](); } }` (and for
  `IteratorObject`, `AsyncIterableIterator`) makes such values `Iterable` / `AsyncIterable`
  through the rule above: converting one boxes the fat pointer, and its iterator is a call
  through the inner value. Converting one to `Iterator<T, E>` is still an error, whose note
  says to call `x[Symbol.iterator]()`.

### Deviations

- `[Symbol.iterator]()` on the builtins returns `Iterator<T>`, not TS's `ArrayIterator<T>` /
  `MapIterator<T>` / `SetIterator<T>` / `StringIterator<T>` (an `Iterable` implementation must
  return exactly `Iterator<T, E>`), so its result is not itself iterable; the prelude classes
  `ArrayIterator<T>` and `StringIterator` are the implementations.
- `Map` and `Set` iterators see the entries as of the call (like `for...of` over a map, which
  iterates `entries()`); JS's are live. A live map iterator would have to survive the map's
  compaction of deleted entries, which renumbers them; `Map.keys()` / `values()` / `entries()`
  stay arrays.
- `IterableIterator<T>` / `IteratorObject<T>` declare `[Symbol.iterator]()` as returning
  `Iterator<T, E>` (no covariant returns), and are two separate interfaces: a value of one does
  not convert to the other.

### Cost

`bench/iter` (`run.sh 7`), LLVM release, best of 7 interleaved runs (Apple M4, shared with
other builds): 200 × 3M array elements summed modulo a prime.

| program | ms | vs array_loop | Node (ms) |
|---|---|---|---|
| array_loop (`for...of` over the array) | 461 | 1.00 | 3564 |
| array_iterable (the array as an `Iterable<i64>` parameter) | 465 | 1.01 | 3100 |

Here `total` is inlined into `main`, where the conversion makes the vtable a known constant,
so the interface calls become direct and the protocol loop runs at the array loop's speed (one
iterator allocation per loop). Where the implementation is not known at the call, each step
is an interface call (gen_value in section 3).

## 7. Consuming iterables and the remaining syntax (#424)

### Consumers

Spread into an array literal or a rest parameter, `Array.from(src[, f])`, array destructuring,
`new Map(src)` / `new Set(src)` and `yield*` take exactly what `for...of` takes: arrays,
strings (characters), `Map`s and `entries()` classes (entries), `Set`s, generators, iterables
and `Iterable<T>` / `IterableIterator<T>` values. None has HIR of its own: each is a `for...of`
loop built by the existing statement code (sema `body/consume.rs`; `is_consumable` is the
shared test): the checked source and a synthesized pattern and body go through `for_iter.rs`
(`iter_source`, then `iter_source_loop`: the embedded loop for a direct generator call, else
the protocol loop) or, for an array, `loops.rs` `for_of`. A string or a map is never iterated
through the protocol (section 6: `is_iterable` skips them): like `for...of`, a consumer takes
the new array of its characters (`split("")`) or entries (`entries()`) and uses it as it is
(`Consumable::into_fresh`): spread copies it as an array source, `Array.from`, destructuring
and `new Map` / `new Set` take it without a loop. The body is source text over
hidden locals (`<array#N>`, `<value#N>`; `#N` numbers them per function), so pushing, mapping
and narrowing are checked as usual:

```text
{ let <array#N> = with_capacity(0);
  for (const <value#N> of src) { <array#N>.push(<value#N>); [if (<array#N>.length >= n) break;] }
  <array#N> }
```

- **Spread** (`expr/spread.rs`): an array literal whose spread sources are all arrays is built
  exactly as before (one allocation of the summed lengths, element loops). An iterable source
  pushes from such a loop at its position (array sources are still evaluated first). A rest
  parameter's arguments are packed into an array literal (`pack_rest`), so `f(...gen())` and
  `Math.max(...values())` need nothing else. Mistyped values (`[...numbers()]` as `string[]`)
  are reported once, without the loop.
- **`Array.from(src)`** collects; **`Array.from(src, f)`** checks `f` against `(T, i64) => ?`
  (its result and error types inferred) and pushes `f(value, i)`; a throw from `f` leaves the
  loop, which closes the iterator, as JS's `IteratorClose`.
- **Destructuring** (`stmt.rs` `var_decl`, `pattern_defaults.rs`, the embedded loop's binding,
  and `loops.rs` for arrays of iterables): an array pattern over a non-array iterable takes
  `collect(src, n)`, at most `n` values (the pattern's length; all of them with `...rest`),
  then destructures that array with the array code (defaults, holes, rest, nested patterns,
  the `index out of bounds` panic for a short one). Breaking at `n` closes the iterator (a
  protocol `return()`, or the embedded generator's drop), so the iterator is closed exactly
  when JS closes it: after the last value the pattern needs, unless it reported `done`. A
  pattern with defaults first binds the source to a hidden `const` (a direct generator call
  is then a generator object). `for (const [a, b] of rows())` desugars the head to
  `const [a, b] = <value#N>` at the start of the body.
- **`new Map(src)` / `new Set(src)`** (`expr/construct.rs`, `args.rs`): their constructors take
  an array (`[K, V][]`, `T[]`); for the prelude's `Map` and std's `Set` an argument that is a
  non-array iterable is collected first (`FnCx::collect_iterable_args`, consumed by the next
  `check_call`). The constructors stay as they are.

**Cost.** `bench/iter` (`run.sh 7`, LLVM release, Apple M4, best of 7): 20 × building a 5M-value
array and summing it.

| program | ms | vs spread_hand |
|---|---|---|
| spread_hand (`xs.push(i)` in a while loop) | 250 | 1.00 |
| spread_gen (`[...range(n)]`) | 247 | 0.98 |

The generator's state is embedded in the filling loop, so the only allocation is the array
(grown by doubling, like the hand loop's pushes). Destructuring allocates the small array of the
values it takes. Every other bench/ program compiles to the same object file as before this
work, with the std at the same path, except `bench/async/all_small_stored.vlt`, whose code is
the same but one internal glue symbol is numbered by a type id (`_Gunclaimed_171`, was `_167`):
the prelude's two new classes intern a few types first.

### Generator function expressions

`function* [name](...): R { ... }` and `async function* ...` are expressions
(`ast::ExprKind::Function`, a contract change; any other `function` expression parses and is
an error asking for an arrow, keeping the stance that arrows are the function expression). Sema
(`expr/gen_closure.rs`) checks one as a closure (`closure.rs`'s machinery) whose `FnDef` has
`is_generator` (and `is_async`): its frame yields `T`, its declared result is normalized like a
`function*` declaration's, it always escapes (the generator outlives the call), and its params
are owned. The value's type is `(...) => R` with the body's error type as `E`; `finalize.rs`
puts the final `E` into the `FnDef`'s result as for declarations. Lowering needed nothing new:
a closure call runs the closure's code, which for a generator `FnDef` builds the generator
object from the env's captures (`ctor_state`, shared with async closures).

- Captures work as for any escaping closure under semantics stage 2: each generator's state
  holds another reference to the captured objects (`take_capture` shares them where an async
  closure deep-copies, since a generator never leaves its thread), and a variable assigned after
  the capture — by the enclosing function or by a generator (`moves` `generator_writes`: each
  generator has its own state, so any assignment needs the cell) — lives in a shared cell whose
  pointer the state holds and releases (`declare_cell_capture`). An async generator expression
  shares likewise. A variable that cannot live in a cell (also captured by an async closure, or
  holding a promise) and that a generator assigns is an error naming `shared(...)`. Like a sync
  closure's captures, a generator closure's are not copied per call where the closure is
  reachable from several threads (an HTTP handler's environment); only resources without
  `clone()` are checked there. The expression's own name is not in scope in its body (a function value cannot
  refer to itself); a use says so. Type parameters and rest parameters are errors.
- At module level, `const g = function* (...) { ... };` is lifted to the generator function `g`
  (`generic_arrows.rs`, next to generic arrow constants), since module constants must be
  constant expressions.

### Iterable object literals

`{ *[Symbol.iterator](): Generator<T> { ... } }` (or `[Symbol.iterator](): Iterator<T> {
return ... }`, or `async *[Symbol.asyncIterator]()`): object literal methods parse as
`ast::ObjectProp::Method` (a contract change). Velt's object literals are plain data, so the
closest workable form is a literal whose *one* member is the iterator method: sema
(`expr/object_method.rs`) makes the method a function value (a generator expression returning
`Iterator<T, E>`, or an arrow) and the object `new __IterableObject(f)`, a prelude class
(`std/prelude/iter.vlt`) implementing `Iterable<T, E>` by calling `f`
(`__AsyncIterableObject` for `AsyncIterable`). Each loop calls the method again, as in JS.

### Deviations

- Object literals: other members next to the iterator method, `this` in it (TS: the object),
  and any other method are errors with the fix (variables, a class implementing `Iterable<T>`,
  or a property holding an arrow). The value is an `__IterableObject<T, E>`, not an object type.
- Generator expressions: no recursion through the name, no type parameters or rest parameters
  (above).
- Spread: array sources are evaluated before the other elements (unchanged); an iterable source
  is iterated where it stands.
- Destructuring an iterable that has fewer values than the pattern panics like a short array
  (JS binds `undefined`), unless the pattern gives defaults.
- `new Set(src)` / `new Map(src)` over an iterable collect the values into an array first (one
  extra allocation) rather than adding them one at a time; the result is the same.
- Destructuring a string or a map builds the whole array of its characters or entries (JS
  stops after the values the pattern needs); nothing can observe the difference, since neither
  runs code per value.

## Follow-ups

- `next(value)` / `throw()` and `return value` (TS's `TNext` / `TReturn`). Today each is an
  error saying that TS allows it, why Velt doesn't, and what to write, as are using the value
  of `yield` / `yield*` and `{ done: true, value: undefined }`.
- Child process stdout/stderr lines and HTTP streaming request/response bodies have chunk pull
  APIs only; a `lines()` method there would follow the same pattern.
- A generator method implementing an interface with an inferred (unwritten) `E` must write it.
- Keeping an embedded async generator's state in registers across steps (section 4, Cost).
- Generators cannot cross tasks; stored `Iterable<T>` values backed by one are checked at run
  time.
- Live `Map` / `Set` iterators (section 6), and `keys()` / `values()` / `entries()` returning
  iterators.
- Interface values converting to the interfaces they extend (section 6), which would also let
  an `IterableIterator<T>` value be an `Iterator<T>`.
