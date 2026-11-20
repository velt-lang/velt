# Iteration, generators and `for await`

Status: implemented. Issue: [#62](https://github.com/velt-lang/velt/issues/62).

## Summary

Velt has TypeScript's iteration protocol: `Iterator<T>`, `Iterable<T>` and their async twins,
`for...of` over any iterable, generators (`function*`), async generators (`async function*`)
and `for await`. Arrays, strings, `Map`s and `Set`s are iterables, std's pull sources (channels,
file and stdin lines, WebSockets, tickers) are async iterables, and spread, `Array.from`, array
destructuring, `new Map(src)` / `new Set(src)` and `yield*` take any iterable.

Two things differ from TypeScript and shape the rest of the design. The protocol types carry a
typed error, `E`, in place of TypeScript's return type: `Iterator<T, E>` is an iterator whose
`next()` throws `E`. And a generator compiles to the same state machine as an async function,
so a `for...of` over a direct generator call keeps the generator's state in the loop's frame
and compiles to the instructions of a hand-written loop.

## Motivation

TypeScript code iterates its own data structures (`for (const x of tree)`), streams lines and
messages (`for await (const line of lines)`), and writes lazy sequences with generators. Without
the protocol, a container has to build an array of its elements to be looped over, and a stream
has to be read with an explicit pull loop:

```ts ignore
while (true) {
  const line = await reader.readLine();
  if (line === null) break;
  handle(line);
}
```

With it, the same loop is `for await (const line of reader.lines()) handle(line);`, and costs
the same.

## Design

### 1. The protocol

The protocol types live in `std/prelude/iter.velt`:

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

- `IteratorResult<T>` is an ordinary discriminated union on a `bool` literal field. `if
  (r.done)` and `if (!r.done)` narrow it (`crates/velt_sema/src/body/narrow.rs`), and a union of
  `bool` literals is a condition (`body/expr/truthiness.rs`).
- The finished result has no `value`. TypeScript's `{ value: undefined, done: true }` is
  accepted and means `{ done: true }`. Reading `r.value` without narrowing is allowed on an
  `IteratorResult<T>` (recognized by its shape, `known.rs` `is_iterator_result`) and gives
  `T | undefined`, `undefined` when done, as in TypeScript (`body/expr/discriminated.rs`
  `union_field`).
- `value` comes first, so a result prints and serializes as in Node (`{ value: 1, done: false
  }`).
- Errors are typed: `E` is what `next()` throws, and `for...of` rethrows it.
- `Iterator.return()` has a default body returning `{ done: true }` (`AsyncIterator.return()`
  an `async` one), so iterators that hold nothing need not write it. TypeScript declares it
  optional (`return?()`). Loops ignore its result.
- `[Symbol.iterator]()` of a class implementing `Iterable<T>` is declared to return
  `Iterator<T, E>`, since an implementation matches the interface signature exactly (Velt has
  no covariant returns). A class that does not declare `implements Iterable` may return its
  concrete iterator class, which `for...of` then calls directly.

**TypeScript's spellings** (`crates/velt_sema/src/ts_protocol.rs`). TypeScript's
`Generator<T, TReturn, TNext>`, `Iterator`, `Iterable`, `IterableIterator`, `IteratorObject` and
the async twins take the return type second. In Velt the second argument is the error type:

| Second argument | Meaning |
|---|---|
| `void`, `undefined`, `unknown` or `any` | dropped: `Generator<number, void>` is `Generator<number>` |
| an error type (a class extending `Error`, a union of them, an interface, a type parameter) | `E` |
| anything else | TypeScript's `TReturn`: an error at the annotation |

The third argument of `Generator` is `TNext`, the type `next(value)` passes in (section 3); a
third argument of the other types is dropped. `IteratorResult<T, TReturn>` with a "returns
nothing" `TReturn` is `IteratorResult<T>`; any other `TReturn` is an error. The check of the
second argument waits until base classes are known.

The protocol relies on these type system features:

- **Type parameter defaults** (`E = never`): the parser's `parse_generic_params` accepts
  `T = Default`, and sema (`resolve.rs` `with_defaults`) fills missing arguments; a default may
  mention earlier parameters.
- **A `throws` clause that names the interface's own type parameters.** A clause on a generic
  interface's method may mention the interface's parameters: each member (implementation,
  default body, override) sees it with the arguments of its own `implements`
  (`throws/groups.rs`, `member_bound`), and a call through an `Iterator<T, E>` value throws the
  clause substituted with the value's type arguments. This is sound because each instantiation
  of a generic interface has its own vtables: `Iterator<string, IoError>` and
  `Iterator<string, never>` entries never mix. HIR carries the slot's error type in
  `InterfaceMethodDef::throws` ([hir_encodings.md](../contracts/hir_encodings.md) "Errors");
  VIR substitutes it at `Callee::Dyn` calls.
- **Inferring `E` from an implementation.** Matching `Iterable<i64, E>` against a class that
  implements `Iterable<i64>` binds `E = never`: interface arguments are matched like error
  types (`infer.rs` `match_iface_args`).
- **Symbol method keys**: `[Symbol.iterator]` and `[Symbol.asyncIterator]` are method names, as
  `[Symbol.dispose]` is (`crates/velt_syntax/src/parser/symbol_keys.rs`).

### 2. `for...of` over an iterable

`for (const x of src)` over an array, a `Map`, a string or a class with `entries()` keeps its
own loop. When `src`'s type has a `[Symbol.iterator]()` method (a class or struct, an
`Iterable<T, E>` value, a bounded type parameter), sema (`body/for_iter.rs`) desugars the
statement before checking it:

```text
{
  let <iterator@N> = src[Symbol.iterator]();        // once
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
  left early (`break`, `return`, a thrown error, a labelled `break` or `continue` of an outer
  statement), and not on normal completion, `continue`, or an error thrown by `next()` itself:
  an iterator whose `next()` threw is finished, as in JavaScript.
- Everything after the first statement is checked like source code, so the usual rules apply:
  narrowing on `done`; typed errors (the `next()` call is an ordinary throwing call, so a
  function containing the loop throws `E`); `try`/`finally` lowering; and ownership (the
  iterator is a local the block owns and drops at its end, and each `value` is moved into the
  binding).
- The hidden names cannot be written in source and are unique per statement.
- A direct generator call (`for (const x of gen(args))`, or a generator `[Symbol.iterator]()`
  method) takes the embedded path of section 3 instead.

### 3. Generators

```ts
function* range(n: i64): Generator<i64> {
  for (let i = 0; i < n; i++) {
    yield i;
  }
}
```

- `function*` (and `*name()`, `static *name()`, `*[Symbol.iterator]()` methods) is a generator.
  Its declared result is required and is `Generator<T, E>`, `Iterator<T, E>` or
  `Iterable<T, E>` (or `IterableIterator<T, E>` / `IteratorObject<T, E>`, `known.rs`
  `SYNC_RESULTS`). A written `E` is a `throws` clause; otherwise `E` is inferred from the body
  like any `throws` (`collect/generator_sig.rs`, `body/generators.rs`). The call only creates
  the generator and never throws.
- `yield` is allowed only in generators, not in an arrow inside one; `await` is not allowed in
  them; `return value` is an error (there is no `TReturn`); a bare `yield` needs
  `Generator<void>`.
- `Generator<T, E, N>` is a prelude **class** (`std/prelude/iter.velt`) implementing
  `Iterator<T, E>`, `Iterable<T, E>`, `IterableIterator<T, E>` and `IteratorObject<T, E>`:
  `next()`, `throw(e)`, `return()`, `[Symbol.iterator]()` (returns `this`) and
  `[Symbol.dispose]()` (closes it, so `using g = gen()` works, and dropping it closes it). Its
  methods use std-only intrinsics (`generatorResume`, `generatorResumeWith`, `generatorThrow`,
  `generatorValue`, `generatorReturn`). `new Generator` and `extends Generator` are errors.
- **`next(value)`.** In a `Generator<T, void, N>` the value of a `yield` expression is the
  argument of the `next(value)` call that resumes it (`const x = yield total;`); the first
  call's value is ignored, as in JavaScript. A `next()` without a value gives `undefined` when
  `N` includes it, and panics otherwise. Without an `N` (`void`, `undefined` or `never`), the
  value of `yield` cannot be used and `next()` takes no argument. A generator with an `N` is
  still a `Generator<T>`, an `Iterable<T>` and spreadable.
- **`throw(e)`** resumes the body with `e` (an `any`) thrown at the paused `yield`: a
  `try`/`catch` around it catches it and the generator goes on; otherwise the generator closes
  (its `finally` blocks run) and `throw()` throws `e`. A generator that has not started or has
  finished throws `e` at once. Code for errors thrown in is emitted only in programs that call
  a method named `throw` (`ts_protocol.rs` `calls_throw`, `Program::generator_throw`).
- `yield* src` is `for (const v of src) { yield v; }` over any iterable; a direct generator
  call takes the embedded path below. Over a `Generator` source in a generator with an `N`, or
  in a program that calls `throw()`, `yield*` also hands the caller's `next(value)`, `throw(e)`
  and `return()` to the inner generator. Closing the outer generator inside the loop closes
  the inner iterator.
- **Widening.** A generator that throws `E` is accepted where a generator that throws a wider
  `F` is expected, as TypeScript allows: the compiler passes its values through a generator of
  the wider type (`generatorWiden` in `std/prelude/iter.velt`), as it widens a promise. An
  error a caller injects with `throw()` leaves the wrapper as it came.

**Compilation** (`crates/velt_vir/src/lower/async_fn/generator.rs`, `gen_object.rs`,
`gen_send.rs`):

- A generator instance compiles like an async function: its poll function, built by the same
  code, is the **resume** function `f$poll(state, cx)`. A plain resume passes a null `cx`; the
  liveness, spilling, frame-slot promotion (`velt_opt` `frame_slots`) and drop machinery apply
  unchanged. It returns `0` DONE, `1` YIELDED (the value in the result region at offset 0: the
  `Ok` payload of `Result<T, E>` when the body throws, else `T`) or `2` THREW (the region holds
  `Err(e)`). Each `yield` stores the value, sets the tag to its suspension `k` and returns 1.
- `next(value)` resumes with `cx` pointing to the caller's value; the resume block of a
  `yield` of type `N` copies it from there. `throw(e)` calls the instance's throw function,
  which sets `THROW_BIT` in the tag and resumes with `cx` pointing to the error; the dispatch
  case `THROW_BIT | k` of a `yield` inside a `try` with a `catch` throws it there, and
  elsewhere runs the close path and reports `3` (rejected), so the caller throws the error.
- **Closing** is the state's drop (`Work::AsyncDrop`: it sets `DROP_BIT` and calls the resume
  function). For a generator, the dispatch case `DROP_BIT | k` of a `yield` runs what
  `return;` at that `yield` runs, every scope's drops *and `finally` blocks*, where async
  cancellation runs drops only. Before the first resume it drops the arguments; after the end
  it does nothing. Both leave the tag DONE, so `next()` after `return()` is done.
- Because closing runs `finally` blocks where the generator cannot pause, report an error or
  continue its body, sema rejects `yield` in a `finally` block, a `finally` block that may
  throw, and a `break` or `continue` in a `finally` block that targets a loop outside it
  (`FnCx::loop_target`, `Frame::finally_loops`).
- While the body runs the tag is `GEN_RUNNING`; resuming or closing the generator from its own
  body panics with `generator is already running` (JavaScript throws a `TypeError`). The body
  cannot free its own state by dropping the last reference to its generator: a call's receiver
  reached through a counted object or a shared cell is kept alive for the statement
  (`crates/velt_vir/src/lower/stabilize.rs`).
- A **generator object** is one heap block `[table: ptr][class fields][state]`: the class's
  first field holds the pointer to the static per-instance table of resume, close and free
  functions (and the throw function, in a program that calls `throw()`; and the state's trace
  glue, `f$trace` at byte 32, in a program with weak references: [weak-refs.md](weak-refs.md)
  "Trace glue"). The state starts
  after the class's fields (`Cx::gen_state_off`). `Work::GenNew` builds it; the class's drop
  runs `[Symbol.dispose]` (close), then frees the block through the table. `console.log(g)`
  prints `Object [Generator] {}` like Node.
- A generator's state cannot be copied, so sema rejects every deep copy of a value holding one
  (`known.rs` `holds_generator`, `body/generators.rs` `no_generator_copy`): `clone()`, an
  argument or receiver of a spawned call, a value sent on a std `Channel`, and a capture of an
  async closure that may run on another thread. A generator converted to an interface value at
  the spawned call itself (`spawn(sum(gen()))` with `sum(it: Iterable<T>)`) is caught too. An
  interface value made earlier is not looked into: one backed by a generator that reaches
  another task panics at run time (`a generator cannot be copied`).
- **Embedded loops**: `for (const x of gen(a))` with a direct call (sema emits
  `GeneratorEmbed`, [hir_encodings.md](../contracts/hir_encodings.md) "Generators") builds the
  state in a frame local, spilled into the enclosing state when the loop is in an async
  function or a generator, calls the resume function directly each step and reads the value
  slot; the local's drop closes it. A generator iterating a direct call of itself (recursion,
  `yield* walk(t.left)`) cannot embed its own state, so that call becomes a generator object.

### 4. Async generators and `for await`

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

- `async function*`, `async *name()`, `static async *name()` and
  `async *[Symbol.asyncIterator]()` are async generators. The declared result is
  `AsyncGenerator<T, E>`, `AsyncIterator<T, E>`, `AsyncIterable<T, E>` or
  `AsyncIterableIterator<T, E>`; a sync result on an `async function*`, or an async one on a
  `function*`, is an error naming the fix. `E` is inferred from the body (awaited calls
  included) or written. The body may `await` and `yield`. A call creates the generator, not a
  promise: HIR `FnDef` has both `is_generator` and `is_async`.
- `AsyncGenerator<T, E>` is a prelude class (`std/prelude/async_generator.velt`): `async
  next()`, `async return()`, `[Symbol.asyncIterator]()` (itself), `async
  [Symbol.asyncDispose]()` (the awaited close) and `[Symbol.dispose]()` (the close without
  awaiting, also its drop), over the std-only intrinsics `asyncGeneratorResume`, `Value`,
  `Return` and `Dispose` ([hir_encodings.md](../contracts/hir_encodings.md) "Async
  generators"). `new AsyncGenerator` and `extends AsyncGenerator` are errors.
- `for await (const x of src)` (`ast::StmtKind::ForOf::is_await`, sema `body/for_await.rs`) is
  allowed only in async functions and async generators. Over a type with
  `[Symbol.asyncIterator]()` it is section 2's protocol loop with `await <it>.next()` and, on
  early exit, `await <it>.return()`. Over a direct async generator call it is the embedded loop
  below. Over a sync source (an array, an iterable, a generator) it is `for...of` with each
  value awaited when the element type is a promise; an array of promises is consumed (its
  promises move out). A sync `for...of` over an async iterable says to use `for await`.
- `yield* src` in an async generator is `for await (const v of src) yield v;`: async
  iterables, and sync ones as in JavaScript.
- `yield p` with `p: Promise<T, E2>` is `yield (await p)`, as in JavaScript (`expr/tasks.rs`
  `yielded_awaiting`): the rejection is thrown at the `yield` and `E2` joins the generator's
  error type.

**Compilation** (`crates/velt_vir/src/lower/async_fn/generator.rs`):

- An async generator instance is an async state machine with both suspension kinds: its poll
  function `f$poll(state, cx)` returns `0` PENDING (an `await` suspended; the waker is
  registered as for any poll), `1` DONE, `2` YIELDED or `3` THREW: the sync generator codes
  plus one (`FnLower::gen_code`), so that 0 stays PENDING. A finished state polled again
  returns DONE.
- `await asyncGeneratorResume(g)` in the consumer is a suspension of the consumer: its resume
  block polls `g` with the consumer's own `cx` (through the object's table, or directly for an
  embedded state); PENDING suspends the consumer, THREW routes the error like a throwing call.
- **Closing.** Tag bit `CLOSE_BIT` (`0x4000_0000`; `DONE | CLOSE_BIT == DONE`) asks for the
  *awaited* close: `Work::AsyncCloseStart` (in the object's table) sets it, and the closer
  polls until not PENDING. The case `CLOSE_BIT | k` of a `yield` runs what `return;` at the
  `yield` runs, including `finally` blocks that `await`. The *dropping* close (`DROP_BIT`,
  which runs with no `cx`) is the same block when that cleanup has no `await`; otherwise it is
  a cancel block (drops only, like a cancelled async function). At an `await` (a `next()` whose
  promise was dropped) both bits cancel the pending child.
- **Embedded loops.** `for await (const x of agen(a))` keeps the state in a hidden local of the
  enclosing state machine and is wrapped in `try { … } finally { await
  asyncGeneratorReturn(<generator>) }`, so leaving the loop early awaits the close (one tag test
  on the normal path, once the generator is done). Cancelling the enclosing task drops the
  local, which runs the dropping close.
- **Heap objects** have the sync layout with the call queue's three words between the table
  pointer and the state; the table has a fourth entry (close-start). `await g.next()` on an
  `AsyncGenerator<T>` variable is a direct call of the class's async method, whose state the
  caller embeds (no allocation per item); through an `AsyncIterator<T>` interface value each
  `next()` is a boxed promise (one allocation per item).

**Overlapping calls.** `AsyncGenerator.next()` and `return()` follow JavaScript's queue: a call
made while another is running waits for its turn, so each promise settles with its own step in
call order, and a call whose promise is dropped still takes its turn. The queue lives in the
prelude class: three `u64` fields after the table pointer (whether a call is running, `head`
and `tail`), holding runtime latches (`velt_rt_latch_*`). A call that finds none running runs
at once and allocates nothing. A call that has to wait creates the latch it will open when it
finishes, leaves it in `tail`, and waits on the latch of the call before it (the previous
`tail`, or `head`, which it creates for a call that started alone). A finished call opens and
frees its latch, which wakes only the next call in line, or, when nobody queued behind it,
clears `running`; so n overlapping calls cost n wake-ups. `next()` and `return()` pass the turn
on in a `finally`. All callers of one generator run on its task (a generator cannot cross
tasks), so the fields need no atomics, and a pending call keeps the object alive, so none is
queued when it is dropped. A call cancelled while it waits for its turn (only when its whole
task is dropped) never opens its latch; everything else on that task is dropped with it. The
embedded `for await` loop over a direct call has no object and no queue.

**Async methods on resources.** `await g.next()` passes `g` as the async method's receiver. An
object receiver or argument of an async call is a soft move like any other object (`tasks.rs`
`note_async_args`, `method_call.rs` `receiver`): it is shared when the place is used again or
is a `using` variable (`ownership/validate.rs`); values holding a promise move. A `using`
variable is disposed at the end of its block whoever else holds it, so sharing one with a
promise that outlives the block would run the call on a disposed object. Sema therefore shares
a `using` variable only with a call awaited where it is made (`awaited_using_shares`); a stored
or returned call is an error (`finish_using_shares`), and `spawn(r.read())` stays "cannot move
`r` out of its `using` declaration", since the task's deep copy would be disposed a second
time. An `await using` variable may be shared with a stored promise: its block awaits
`[Symbol.asyncDispose]()` at the end, as JavaScript does.

### 5. Std sources

Each std pull source is also an async iterable; the pull methods stay. The iterator is an async
generator method, so a direct `for await` over the source embeds the generator's state in the
caller (section 4): no allocation per item, the same runtime calls as the pull loop.

| Source | How | Yields | `E` |
|---|---|---|---|
| `Channel<T>` (`std/sync/channel.velt`) | `implements AsyncIterable<T>` | values until closed and drained | `never` |
| `FileReader` (`std/fs/file_reader.velt`) | `async *lines(): AsyncGenerator<string, IoError>` | remaining lines | `IoError` |
| `velt/process` (`std/process/stdin.velt`) | `export async function* lines()` | remaining lines of stdin | `IoError` |
| `WebSocket` (`std/net/websocket/socket.velt`) | `implements AsyncIterable<WsMessage, IoError>` | messages until the peer closed | `IoError` |
| `RedisSubscriber` (`packages/redis/src/subscriber.velt`) | `implements AsyncIterable<RedisMessage, RedisError>` | messages until `close()` | `RedisError` |
| `Ticker` (`std/time/ticker.velt`) | `implements AsyncIterable<i64>` | tick numbers 1, 2, … until stopped | `never` |
| `CopyReader` (`packages/postgres/src/copy_reader.velt`) | `implements AsyncIterable<string, PgError>` | `read()` chunks to the end | `PgError` |

`FileReader` and stdin have a `lines()` method rather than being iterable themselves: a reader
also reads bytes and chunks, so the line view is named, like Node's `filehandle.readLines()`.

**Early exit does not close the source.** Leaving the loop closes only the generator, which
holds nothing: the channel, socket, subscription or reader stays open, and the next value is
still receivable. A channel has other receivers and these handles are shared copies, so ending
one consumer's loop must not end everyone's stream; closing is an explicit `close()` or
`stop()`. The generators are suspended at a `yield` whenever the loop body runs, so no
received value is lost by a `break`.

**Generator methods in dispatch groups** (`throws/groups.rs`, `collect/impls.rs`). A generator
method implementing `[Symbol.asyncIterator](): AsyncIterator<T, E>` is checked against the
interface with its written `E` restored into its result (a missing one is reported with
"declare the generator's result as `AsyncIterator<T, E>`"). Generator methods are not members
of dispatch groups: calling one never throws, and its `E` lives in the result type, so
implementations with different error types (`IoError`, `PgError`, `RedisError`) do not merge.

### 6. Builtin iterables and TypeScript's iterator types

An `extend` block defining `[Symbol.iterator](): Iterator<T, E>` makes its target implement
`Iterable<T, E>`, and one defining `[Symbol.asyncIterator](): AsyncIterator<T, E>` makes it an
`AsyncIterable<T, E>` (`collect/iterable.rs`: an entry in `Program::impls`, generic when the
block is). This applies to user blocks too, and to a generator method: one returning
`Iterator<T, E>` is the impl's entry itself, and one returning `Generator<T, E>` gets a
synthesized adapter (`collect/forwarders.rs` `Target::Generator`) that returns the generator as
an `Iterator<T, E>`. A generator method implementing an interface method whose error type is
not written must not throw (`throws/checks.rs` `iface_generator`), since the impl's `E` comes
from the result as written, before bodies are checked; a throwing one is an error asking to
write `E`. The prelude defines:

| Type | `[Symbol.iterator]()` returns | Iterates |
|---|---|---|
| `T[]` | `new ArrayIterator<T>(this)` | a live view: the iterator shares the array and reads the length at each step; once done it stays done |
| `string` | `new StringIterator(this)` | characters (code points) as strings |
| `Map<K, V>` | `new ArrayIterator(this.entries())` | the entries as of the call |
| `Set<T>` (`std/collections/set.velt`) | `new ArrayIterator<T>(this.values())` | the elements as of the call (`Set` declares `implements Iterable<T>`) |

So `[1, 2, 3]` converts to an `Iterable<i64>` value (the array's header is boxed unless the
array is already counted; no element is copied), satisfies an `Iterable<T>` bound, and
`a[Symbol.iterator]()` is an ordinary extension call. An array literal written where an
`Iterable<T>` is expected takes `T` as its element type (`sum([1, 2])` with
`sum(xs: Iterable<f64>)`; `known.rs` `iterable_elem`).

`for...of` over an array, a string or a `Map` keeps its own loop (`body/for_iter.rs`
`is_iterable` skips them); only code going through `Iterable<T>` (a value or a bound) uses the
protocol. `forEach` and `for...of` over a map, or over its `keys()`, `values()` or `entries()`,
are live: they walk the entry positions with a cursor that finds its place again when a
compaction, a pop or a clear renumbers them (`std/lang/map/layout.velt`, "Live iteration";
`body/for_map.rs`).

TypeScript's iterator types are prelude interfaces:

```ts ignore
interface IterableIterator<T, E = never> extends Iterator<T, E>, Iterable<T, E> {}
interface IteratorObject<T, E = never> extends Iterator<T, E>, Iterable<T, E> {}
interface AsyncIterableIterator<T, E = never> extends AsyncIterator<T, E>, AsyncIterable<T, E> {}
```

`Generator`, `ArrayIterator` and `StringIterator` implement the first two, `AsyncGenerator` the
third, and generators may be declared to return them. Their second argument follows the rules
of section 1. An interface value converts to the interfaces it extends, keeping the same object
(`crates/velt_sema/src/synth_ifaces/upcasts.rs`, [hir_encodings.md](../contracts/hir_encodings.md)
"Interface intersections and upcasts").

### 7. Consumers, generator expressions and iterable object literals

**Consumers.** Spread into an array literal or a rest parameter, `Array.from(src[, f])`, array
destructuring, `new Map(src)` / `new Set(src)` and `yield*` take exactly what `for...of` takes:
arrays, strings (characters), `Map`s and `entries()` classes (entries), `Set`s, generators,
iterables and `Iterable<T>` / `IterableIterator<T>` values. None has HIR of its own: each is a
`for...of` loop built by the statement code (`body/consume.rs`; `is_consumable` is the shared
test), through `for_iter.rs` (`iter_source`, then `iter_source_loop`: the embedded loop for a
direct generator call, else the protocol loop) or, for an array, `loops.rs`. A string or a map
is never iterated through the protocol: a consumer takes the new array of its characters or
entries and uses it as it is (`Consumable::into_fresh`). The loop body is source text over
hidden locals, so pushing, mapping and narrowing are checked as usual:

```text
{ let <array#N> = with_capacity(0);
  for (const <value#N> of src) { <array#N>.push(<value#N>); [if (<array#N>.length >= n) break;] }
  <array#N> }
```

- **Spread** (`expr/spread_array.rs`): an array literal whose spread sources are all arrays is
  one allocation of the summed lengths and element loops. An iterable source pushes from such a
  loop at its position. Elements are evaluated in written order: an array source that is not a
  place is evaluated before the allocation only when that moves no effects past an element that
  can observe them (`expr/eval_effects.rs`); otherwise it is evaluated at its position and left
  out of the allocation's size. A rest parameter's arguments are packed into an array literal
  (`args.rs` `pack_rest`), so `f(...gen())` and `Math.max(...values())` need nothing else.
- **`Array.from(src)`** collects; **`Array.from(src, f)`** checks `f` against `(T, i64) => ?`
  and pushes `f(value, i)`; a throw from `f` leaves the loop, which closes the iterator, as
  JavaScript's `IteratorClose` does.
- **Destructuring** (`stmt.rs`, `pattern_defaults.rs`, `loops.rs`): an array pattern over a
  non-array iterable takes at most `n` values (the pattern's length; all of them with
  `...rest`), then destructures that array with the array code (defaults, holes, rest, nested
  patterns, the `index out of bounds` panic for a short one). Breaking at `n` closes the
  iterator, so it is closed exactly when JavaScript closes it. `for (const [a, b] of rows())`
  desugars the head to `const [a, b] = <value#N>` at the start of the body. Nested array
  patterns over iterables (`const [[p, q], [r]] = [gen(), gen()]`) are split off
  (`body/nested_pattern.rs`): the outer pattern binds each such value to a hidden local, which
  a declaration then takes apart.
- **`new Map(src)` / `new Set(src)`** (`expr/construct.rs`): the constructors take an array;
  an argument that is a non-array iterable is collected into one first
  (`FnCx::collect_iterable_args`).

**Function expressions.** `function* [name](...): R { ... }` and `async function* ...` are
expressions (`ast::ExprKind::Function`); other `function` expressions are checked as the arrow
with the same parameters and body. Sema (`expr/gen_closure.rs`) checks a generator expression
as a closure whose `FnDef` has `is_generator` (and `is_async`): its declared result is
normalized like a `function*` declaration's, it always escapes (the generator outlives the
call), and its parameters are owned. Its type is `(...) => R` with the body's error type as
`E`. Lowering builds the generator object from the closure environment's captures, as for
async closures.

- Each generator's state holds another reference to the captured objects (a share, not a
  copy: a generator never leaves its thread). A variable assigned after the capture, by the
  enclosing function or by a generator (`moves` `generator_writes`), lives in a shared cell
  whose pointer the state holds (`declare_cell_capture`). A variable that cannot live in a cell
  (also captured by an async closure, or holding a promise) and that a generator assigns is an
  error naming `shared(...)`.
- Without a written result type, a generator expression takes the result of the function type
  expected where it stands, like an arrow; elsewhere its yield type is inferred from its
  `yield` values, joined as an arrow's returns are (`body/yield_infer.rs`), and a `yield*`
  there needs the result type written.
- At module level, `const g = function* (...) { ... };` is the generator function `g`
  (`crates/velt_sema/src/generic_arrows/`), since module constants must be constant
  expressions.

**Generator methods in object literals.** `*name()` and `async *name()` in an object literal
are properties holding generator functions, as in Node.

**Iterable object literals.** `{ *[Symbol.iterator](): Generator<T> { ... } }` (or
`[Symbol.iterator](): Iterator<T> { return ... }`, or `async *[Symbol.asyncIterator]()`) is an
`Iterable<T, E>`. Velt's object literals are data, so a literal whose *one* member is the
iterator method becomes `new IterableObject(f)` (`expr/object_method.rs`), where `f` is the
method as a function value and `IterableObject` (`std/lang/hooks/iter.velt`;
`AsyncIterableObject` in `async_iter.velt`) implements `Iterable<T, E>` by calling it. Each
loop calls the method again, as in JavaScript. A literal implementing an interface with methods
(`IterableIterator<T>`, with `next()` next to `[Symbol.iterator]()`) is an object of that
interface instead, and `this` in its methods is the object.

## Runtime cost

Measured with `bench/iter` (`bench/iter/run.sh`, LLVM release, best of 7 interleaved runs on an
Apple M4; identical programs vary by up to ±10% between rounds on a busy machine). The numbers
are also in `bench/RESULTS.md`.

20 × 30M values summed modulo a prime:

| program | ms | vs hand loop |
|---|---|---|
| hand_loop (while loop) | 1172 | 1.00 |
| gen_loop (`for...of` over `range(n)`) | 1233 | 1.05 |
| iterable_class (iterator class) | 1147 | 0.98 |
| gen_value (`Iterable<i64>` parameter) | 2997 | 2.56 |

The embedded loop compiles to the hand loop's instructions (the same seven per value, rotated):
the resume function is inlined, the state is promoted to registers and the dispatch switch is
jump-threaded away; the difference in the table is noise. A loop the vectorizer handles as a
hand loop stays scalar as a generator, since the `yield` is an exit from the producer's loop.
A generator value costs an interface call and a table call per step. A protocol loop costs one
`[Symbol.iterator]()` call per loop and, per element, one `next()` call, a tag test on the
result (no allocation) and two stores to a local flag; iterating an `Iterable<T>` value boxes
its iterator once per loop.

20 × 3M values, each from an async call that completes at once (`await step(i)`):

| program | ms | vs async_hand | Node (ms) |
|---|---|---|---|
| async_hand (while loop, `await step(i)`) | 49 | 1.00 | 1876 |
| async_gen (`for await` over `values(n)`) | 118 | 2.41 | 7043 |

Neither allocates per value. The hand loop inlines `step` and keeps everything in registers
(about 0.8 ns per value); the generator loop costs about 1.2 ns more per value, because the
generator's state lives in the caller's state memory (an async function's state is not
promoted to registers across its suspension points), so each step loads and stores the
generator's tag and counter, polls it and tests the result codes. A body doing real work hides
this.

`for await (const v of ch)` over a `Channel` calls the same `Channel.receive` as
`await ch.receive()`, with no allocation per value: `bench/async/channel_pipeline.velt` (1M
values from 4 producers, `VELT_THREADS=1`, best of 21) takes 44.6 ms with the `receive()` loop
and 46.8 ms with `for await`.

200 × 3M array elements summed: `for...of` over the array 461 ms, the array as an
`Iterable<i64>` parameter 465 ms (Node: 3564 and 3100 ms). There the callee is inlined, the
conversion makes the vtable a known constant, and the interface calls become direct. Where the
implementation is not known at the call, each step is an interface call. `ArrayIterator`
shares the array, so an array type whose `[Symbol.iterator]()` is instantiated becomes counted
program-wide, like any shared array; programs that don't iterate arrays through `Iterable<T>`
keep their representation.

20 × building a 5M-value array: `xs.push(i)` in a while loop 250 ms, `[...range(n)]` 247 ms.
The generator's state is embedded in the filling loop, so the only allocation is the array.

## Diagnostics

| Code | Message |
|---|---|
| `for (const x of 5)` | `` cannot iterate over a value of type `f64` ``, noting what `for...of` accepts |
| iterating an iterator | the same, noting that the value looks like an iterator and to iterate the iterable that creates it |
| `[Symbol.iterator](): Counter[]` | `` `[Symbol.iterator]()` must return an `Iterator<T>`, found `Counter[]` `` |
| a loop over `Iterable<T, Read>` in a function `throws Other` | `` `f` throws `Read`, which its `throws` clause does not allow `` |
| `next()` throwing in `implements Iterator<i64>` | `` `C.next` throws `Read`, but `Iterator.next` does not allow it `` |
| `function* g() { ... }` | `a generator must declare its return type` |
| `return 2;` in a generator | `a generator cannot return a value` |
| `const x = yield 1;` without `N` | `` the value of `yield` cannot be used ``, suggesting `Generator<T, void, N>` |
| `g.next(5)` without `N` | `` `next()` takes no argument `` |
| `const r = yield* inner();` | `` the value of `yield*` cannot be used `` |
| `yield` in a `finally` block | `` `yield` cannot be used in a `finally` block `` |
| a `finally` block that may throw | `` a `finally` block in a generator cannot throw, but this one may throw `Boom` `` |
| `break` out of a `finally` block | `` `break` cannot leave a `finally` block in a generator `` |
| `Generator<number, string>` | `` the second type argument of `Generator` is the error type it throws, and `string` is not an error type `` |
| `Generator<number, string, unknown>` | `` `Generator` takes no return type: `string` is TypeScript's `TReturn` `` |
| `clone()` of a generator | `a generator cannot be copied` |
| a generator passed to `spawn` | `` a generator (`Generator<i64, never>`) cannot be passed to another task `` |
| `for await` in a sync function | `` `for await` is only allowed inside async functions `` |
| `new Generator()` | `` generators cannot be created with `new` `` |
| an iterable literal with other members | `` an object literal with a `[Symbol.iterator]()` method cannot have other members `` |
| `this` in an iterable literal's method | `` `this` cannot be used in the `[Symbol.iterator]()` method of an object literal `` |

Each note of a TypeScript form Velt rejects says that TypeScript allows it, why Velt does not,
and what to write.

## Compatibility

- The second type argument of the protocol types is the error type, not TypeScript's
  `TReturn`; TypeScript's "returns nothing" spellings are accepted (section 1).
- `Generator<T, E, N>` is a class, not an interface, and `[Symbol.iterator]()` on generators
  and builtins returns `Iterator<T, E>`, not TypeScript's `ArrayIterator<T>` / `MapIterator<T>`
  and so on (Velt has no covariant returns).
- A `finally` block in a generator cannot `yield`, throw, or `break` / `continue` out of
  itself. `finally` blocks of an async generator may `await` when it is closed with `return()`;
  an async generator dropped without `return()` cancels such cleanup instead of running it
  (JavaScript never runs it at all).
- `for await` over a sync source awaits only promise elements; it adds no suspension per
  element, so a loop over plain values does not yield to other work as JavaScript's would.
- Leaving a loop over a std source does not close the source (Node's stream iterators destroy
  the stream).
- `Map` and `Set` iterators from `[Symbol.iterator]()` see the entries as of the call;
  JavaScript's are live. `Map.keys()`, `values()` and `entries()` return arrays.
- Destructuring an iterable that has fewer values than the pattern panics like a short array
  (JavaScript binds `undefined`), unless the pattern gives defaults. With nested patterns, the
  outer pattern takes all of its values before an inner one takes its own, which only an outer
  generator with effects can tell apart.
- `new Set(src)` / `new Map(src)` over an iterable collect the values into an array first (one
  extra allocation); the result is the same.

## Limitations

- Generators return no value: `return value`, the value of `yield*`, and a `TReturn` type
  argument are errors.
- `AsyncGenerator` has no `next(value)` and no `throw()`.
- A generator cannot be passed to another task; an `Iterable<T>` interface value backed by one
  is checked only at run time.
- A generator function expression cannot use its own name (no recursion through it), and
  cannot have type parameters or rest parameters.
- An iterable object literal holds only its iterator method: other members, `this` in the
  method, and parameters on it are errors, unless the literal implements an interface with
  methods.
- Child process stdout/stderr and HTTP streaming bodies have chunk pull APIs only, without
  `lines()`.
- An embedded async generator's state is not kept in registers across steps (see "Runtime
  cost").
