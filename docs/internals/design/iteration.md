# Design: iteration, generators and `for await`

Status: accepted (issue #62), implemented in four phases, **all built**: 1 (the protocol and
`for...of` over user iterables), 2 (sync generators), 3 (async generators and `for await`) and
4 (std sources are async iterables).

## Problem

TypeScript code iterates its own data structures (`for (const x of tree)`), streams lines and
messages (`for await (const line of lines)`), and writes lazy sequences with generators
(`function*`). Before this work, Velt's `for...of` only knew arrays, maps and classes with an
`entries()` method that builds an array, and there was no generator or async iteration at all.

## 1. The protocol (built, phase 1)

```ts ignore
type IteratorResult<T> = { done: false; value: T } | { done: true };

interface Iterator<T, E = never> {
  next(): IteratorResult<T> throws E;
  return(): void {}                       // early exit: release what the iterator holds
}
interface Iterable<T, E = never> {
  [Symbol.iterator](): Iterator<T, E>;
}

interface AsyncIterator<T, E = never> {
  next(): Promise<IteratorResult<T>, E>;
  async return(): Promise<void> {}
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
- Errors are typed: `E` is what `next()` throws. `for...of` rethrows it.
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

- `Iterator.return()` has an empty default body (`AsyncIterator.return()` an empty `async` one),
  so iterators that hold nothing need not write it; TS declares it optional (`return?()`).
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
| `next()` returning `{ done: false, value: 1 }` for `Iterator<string>` | the usual type mismatch at the literal |
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
- While the body runs the tag is RUNNING; resuming then (the body reaching its own generator)
  panics with `generator is already running` (JS throws a `TypeError`). The store is dead in
  an inlined loop and disappears.
- A **generator object** is one heap block `[table: ptr][state]` (the class's one field is the
  table pointer; states are at most 8-aligned): the static per-instance table holds resume,
  close and free functions. `Work::GenNew` builds it (counted like any object of the class when
  `Generator<T, E>` is shared), `Work::Fn` returns it or, for a declared `Iterator` / `Iterable`
  result, its interface value; the class's drop runs `[Symbol.dispose]` (close), then frees the
  block through the table (`object_free`). `console.log(g)` prints `Object [Generator] {}`
  like Node. A generator's state cannot be copied, so sema rejects every deep copy of a value
  holding one (`known.rs` `holds_generator`, `body/generators.rs` `no_generator_copy`):
  `clone()`, an argument (or receiver) of a spawned call, a value sent on a std `Channel`, and
  a capture of an async closure (whose state copies its captures when it runs), each with
  "a generator cannot be copied" / "... cannot be passed to another task" / "an async closure
  cannot capture the generator `g`". Interface values are not looked into: an
  `Iterable<T>` value backed by a generator that reaches a spawned task still panics at run
  time (`a generator cannot be copied`, the clone glue).
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
- **Heap objects** are the sync layout `[table][state]`; the table has a fourth entry
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

## Follow-ups

- `next(value)` / `throw()` and `return value` (TS's `TNext` / `TReturn`).
- Child process stdout/stderr lines and HTTP streaming request/response bodies have chunk pull
  APIs only; a `lines()` method there would follow the same pattern.
- A generator method implementing an interface with an inferred (unwritten) `E` must write it.
- Keeping an embedded async generator's state in registers across steps (section 4, Cost).
- Generators cannot cross tasks; `Iterable<T>` values backed by one are checked at run time.
