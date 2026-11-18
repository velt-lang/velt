# JavaScript semantics without a garbage collector

Status: partly implemented. Issues: [#3119](https://github.com/velt-lang/velt/issues/3119), [#11](https://github.com/velt-lang/velt/issues/11).

## Summary

TypeScript developers never write `.clone()` or see "use of moved value": strings are values,
and objects, arrays, maps and closures are shared references, as in JavaScript. Velt keeps
Rust-level performance and memory behavior by paying for sharing only where a program shares:
the compiler infers which values have a single owner and compiles them exactly as unique,
uncounted values; only the types a program actually shares get a reference count.

Three requirements hold for every part of this design:

1. **No tracing garbage collector over the whole heap.** No background heap scanning and no
   heap-size tuning.
2. **No pauses.** Memory is freed deterministically at the last use (a count reaching zero, or
   the end of unique ownership).
3. **No performance loss.** Changes to the memory model are gated by the benchmark suite
   ("Runtime cost").

## Design

### The model

| Kind | Semantics (user-visible) | Implementation |
|---|---|---|
| numbers, `bool`, Copy `struct`s | values (JS primitives) | registers and stack |
| `string` | immutable value; copy freely | inline up to 23 bytes; longer ones in a counted immutable buffer; a copy is a count increment, elided when the source is dead (a move) or when the copy is only read while nothing can change the source (a borrow, `velt_opt` `borrowed_copies`) |
| class instances, arrays, `Map`, object literals, closures | **shared references** (`const b = a; b.push(1)` changes `a`) | a value with a provably single owner keeps unique code (no count, `noalias`); the types the program shares get a count |
| values crossing threads (`spawn`, handlers, channels) | transferred: moved when unique, deep-copied when still shared | non-atomic counts; the transfer glue keeps every counted object on one thread |
| `x.clone()` | explicit independent deep copy (like `structuredClone`) | deep copy |

So `.clone()` is never needed for strings, getters returning fields, `for...of` elements,
`Map.get` results or literals built from borrowed data; there is no "use of moved value"; and
`let count = 0; const inc = () => count++` works (a captured variable that is assigned gets a
counted cell only when the closure escapes).

`[Symbol.dispose]()` runs when the last reference goes away; `using x = …` disposes at the end
of the enclosing block, and `await using` with `[Symbol.asyncDispose]()` works the same way.

### Identity and equality

- `==` / `===` on objects (class instances, arrays, object literals, interface values and
  function values) compares **identity**, as in JavaScript.
- `deepEqual(a, b)` (prelude) compares contents; `assertEq` uses it.
- `Map` and `Set` keys compare with SameValueZero (identity for objects, `NaN` equal to itself)
  and hash an object by its identity.
- Function values compare code and environment; each evaluation of a closure expression is a
  new function, as in JavaScript.
- **`struct` declares an immutable value type**, the one exception: every field is readonly
  (`p.x = …` is an error whose note suggests a spread), it has no constructor, no
  `[Symbol.dispose]` and no `Name { … }` literal (an object literal where the type is known, or
  `{ … } as Name`). A struct has no identity: `==`, `===`, `includes` and `Map`/`Set` keys
  compare its content (numbers by their bits, so a `NaN` equals itself when the bits match and
  `-0` is not `0`; strings by text; objects inside by identity), and the hash agrees. A struct
  whose fields are all Copy is Copy. The prelude's `Mutex`, `RwLock`, `Atomic` and `ThreadLocal`
  are declared with `struct` to be stored inline but are places, not values: none of this
  applies to them.

### Strings

Strings are immutable values. The representation (inline up to 23 bytes, static, or a heap
buffer with an atomic count) is in [rt_abi.md "Strings"](../contracts/rt_abi.md), and their
JavaScript semantics (UTF-16 code units) in [JavaScript string semantics](strings.md). One
scheme suffices for all threads because the count only moves when a string is copied while its
source stays alive, and dropping the only reference frees after a plain load. Sema makes every
string move "soft" (a move at the last use, a copy otherwise; closures capture by copy), and
`s.clone()` compiles as a plain copy.

`string + x` converts the other operand as `String(x)` does (`"k" + 1` is `"k1"`, an array is
joined, an object uses its `toString()`), as in JavaScript.

### Numbers

`/` on `number` operands is JavaScript division (`const a = 7; a / 2` is `3.5`). On operands of
declared integer types (`i64`, `u8`, …) it is integer division (`n / m` is `3` for `n: i64 = 7`,
`m: i64 = 2`). Lengths and other integers from the standard library are `number`s in user code.
The rules are in [the Reference](https://veltlang.com/docs/reference/types/#numbers).

### Promises

Promises are **hybrid**, with JavaScript's results:

| Code | Behavior | Cost |
|---|---|---|
| `await f()` | runs inline | no task, no allocation |
| `const a = f()` … `await a` | starts immediately, like JS | one small local task |
| `f();` (promise dropped) | compile error "floating promise: this promise is neither awaited nor spawned", with the fixes `await f()` or `spawn(f())`; top-level statements of the root script are exempt | — |
| `Promise.all([f(), g()])` | concurrent | one allocation per promise |

Eagerly started promises run as **local tasks on the current worker thread** (JavaScript's
single-threaded concurrency model), so they need no thread-safety rules. Only `spawn(...)` moves
work to another core. `Promise.all`, `Promise.race`, `Promise.allSettled` and `Promise.any`
match JavaScript.

- A stored promise is a boxed state machine. The call site starts it, which runs it to its first
  suspension and registers it in the **local set** of the task being polled. Every task root
  (`block_on`, spawned tasks, HTTP handlers) polls its woken local promises before its own state
  machine, one at a time and never concurrently with the task. When a local promise finishes,
  its awaiter resumes immediately, like a microtask, so output orderings match Node. A task that
  never stores a promise pays two thread-local writes per poll
  ([rt_abi_async.md §1.1](../contracts/rt_abi_async.md)).
- A promise dropped unawaited runs to completion and its result is dropped. If its task's root
  finishes first, the set moves to an orphan task that keeps the process alive until it is done
  (as Node waits for pending work). Only a cancelled task (an HTTP request whose client left)
  cancels its unfinished local promises.
- Errors are typed ([TypeScript alignment](ts-alignment.md#errors-typed-throw-try-and-catch)).
  A started promise keeps its result or error, so `await p` rethrows. A rejection nobody can
  observe (the promise was dropped unawaited) is reported when it happens, like an unhandled
  rejection (`Uncaught X: msg`, exit code 1).
- `new Promise((resolve, reject) => …)` compiles to the prelude's `promiseNew`: a settle-once
  slot in `shared<Mutex<…>>` plus a runtime latch; `resolve` and `reject` are heap closures over
  it, which sema lets a literal executor keep (`FnInfo::keeps_fn_params`). A promise abandoned
  unsettled never settles, and a direct `await` of it is reported with the `new Promise` site
  (`Intrinsic::SourceLocation`). `Promise.withResolvers<T, E>()` is the prelude's
  `promiseWithResolvers`: the same slot, with `resolve` and `reject` returned as fields.

### `null`, `undefined` and truthiness

- `T | undefined` is its own type with the layout of `T | null`
  ([hir_encodings.md "Undefined"](../contracts/hir_encodings.md)); `typeof`, comparisons,
  printing and JSON follow Node. Optional fields and parameters (`a?: T`) are `T | undefined`,
  and the standard library returns `undefined` where lib.d.ts does (`Map.get`, `find`, `at`).
- Conditions and `!`, `||`, `&&` test values of any type with JavaScript's rules (`0`, `-0`,
  `NaN`, `""`, `null` and `undefined` are falsy; objects are always truthy), and `||` / `&&`
  return an operand typed as TypeScript types it (`n || 5` is a `number`). Each test compiles to
  one comparison (`crates/velt_sema/src/body/expr/truthiness.rs`).

### Modules

`import * as ns`, `export { x } from`, `export * from`, `export * as ns from`, `import type`,
folders through `index.velt` and `paths` aliases in `package.velt` work as in TypeScript.
`export default` works in every file: the parser desugars it into a named export `default`
(`crates/velt_syntax/src/parser/default_exports.rs`). `export default expr` is a constant kept in
module state, evaluated once in module order; `export default x` is too when `x` may change or
is not there yet (a `let`, an imported `let`, a declaration below it, or no module binding).
`export *` leaves `default` out. `export default interface` may stand next to a value default,
as its type meaning, but not next to `export default expr` (TypeScript's TS2528). Namespaces
nest at any depth, cycles included (`a.b.a.x`). The standard library uses named exports only
(`scripts/std-lint`).

### Reference cycles

A count never reaches zero inside a cycle. The compiler knows which types can form one: a block
can only be in a cycle if the "may own a strong reference to" graph of the program's types
reaches its type again (`class Node { next: Node | null }`, parent and child, a listener closure
stored in the object it captures). Uniquely owned values never can.
`VELT_DEBUG_COUNTED=1` prints a build's counted and cycle-capable types
(`crates/velt_vir/src/lower/passes.rs`). `WeakMap`, `WeakSet` and `WeakRef` are in the language
([weak-refs.md](weak-refs.md)). Freeing cyclic garbage is designed in [cycles.md](cycles.md)
("Not yet implemented").

## Compilation

### Where shares come from

Sema keeps its ownership inference (moves at the last use, borrows for calls, inferred `Owned`
parameters) and does not report moves. Wherever a non-Copy place would be used after a move, or
moved out of something it cannot leave (a borrowed parameter, an array element, a class field, a
capture), the use becomes an explicit **share** (`Intrinsic::Share`): a second reference to the
same value. `Intrinsic::Clone` means deep copy: `x.clone()`, and the captures of async closures
that may run on other threads.

- **Shared values** (`Ctx::is_shared_value`): everything that is not Copy (strings, objects,
  arrays, maps, closures, interface values, promises). Object literals are objects and never
  Copy. A share of a promise is another holder of the same future (`velt_rt_fut_share`,
  [rt_abi_async.md §1.2](../contracts/rt_abi_async.md)).
- **Soft moves** (`crates/velt_sema/src/ownership/soft.rs`): a move stays a move where the
  place is dead and becomes a share where it is used again or cannot be moved from. By-value
  captures of escaping closures become share captures (`Capture::share`). The receiver of an
  explicit `x[Symbol.dispose]()` and a `using` declaration stay moves.
- **Implicit copies are shares**: async-call arguments used again, discriminated-union field
  reads, spread fields, interface-field getters, a borrowed place used as an owned value, and the
  prelude's `filter`, `slice`, `at`, `find`, `concat`, `Map.get`/`keys`/`values`/`entries`,
  `fill` and `getOrInsert`: `xs.filter(f)` returns the same objects.
- **By-reference `const`s** (`const x = node.left`) stay borrows unless the rest of the block
  would invalidate the place; then they become owned shares instead of an error
  (`ownership/exclusive/let_borrow.rs`). Writes through pattern, `for...of` and `const` bindings
  are attributed to the place they point into.
- A parameter that stores or returns its value is `Owned`, so a caller whose variable is dead
  hands it over for free, and only a caller that uses its variable again pays one increment.
- `==` / `!=` on non-primitive types is `Intrinsic::Same` (JavaScript's `===`); `Map` and `Set`
  keys compare with `Intrinsic::KeyEq` (`Same` with `NaN` equal to itself).

### Which types are counted

Lowering (`crates/velt_vir/src/lower/boxing/`) records **facts** while it builds functions:
every share of a value of type `T`, every borrow of a place reached through a counted value
("Borrows through counted objects"), every identity comparison. `lower_program` repeats
lowering with the closure of those facts until it is stable (the type table carries over between
passes, so type ids stay valid). A program without shares lowers once. `ShareKind`
(`boxing/kinds.rs`):

| Type | Share | When shared |
|---|---|---|
| numbers, `bool`, literal and C-like enum types, structs of such fields | bitwise copy | — |
| `string` | `velt_rt_str_clone` | — |
| class instance | count + 1 | the class hierarchy (by root class) gets a count in front of every object |
| `T[]` | count + 1 | **boxed**: values become a pointer to the `{ data, len, cap }` header in a counted heap block |
| object literal type that is assigned somewhere (`Program::assigned_objects`), has `[Symbol.dispose]` or holds a `Mutex` | count + 1 | boxed like arrays (a pointer to the fields) |
| other object literal types, `T \| null`, unions, tuples | part-wise share (`Glue::Share`) | parts as needed; an object type compared by identity is boxed instead |
| function value | environment count + 1 (heap environments are always counted; a frame environment is copied to the heap) | — |
| interface value | the data's share entry in the vtable (`SLOT_SHARE`) | — |
| `shared<T>` | its own atomic count | — |

Hashing an object type or an array as a `Map` key counts it, so its pointer is the object
wherever the map moves it. Granularity is per type: one share of `T[]` anywhere counts every
`T[]` of the program.

**Why the count lives at `-8`:** a pointer to a boxed value is exactly the pointer the borrow
ABI passes for an inline value (`&T`). Borrowed parameters, `this`, glue functions and externs
taking `T*` work unchanged; only owning storage (locals, fields, elements, captures, results)
holds a pointer instead of the inline bytes, and content accesses go through `FnLower::content`.

### Operations

In `crates/velt_vir/src/lower/rc.rs`, `boxes.rs` and `share.rs`:

- **share**: `count += 1`, non-atomic, inline, no runtime call;
- **release**: `if count == 1 { drop the contents; free } else { count -= 1 }`: the unique case
  reads the count and never writes it; `[Symbol.dispose]()` runs as part of dropping the
  contents;
- **deep clone** (`x.clone()`): a fresh box (count 1) with a deep copy of the contents; shared
  substructures are duplicated (`structuredClone` keeps the aliasing within the copy);
- **move**: copies the pointer.

### Borrows through counted objects

In unique code the exclusivity rule guarantees that a value borrowed by a call stays put. A
place reached through a counted object or a cell has other owners: the callee may reach the same
object another way and replace or free what the borrow points to. Such borrows are
**stabilized** (`crates/velt_vir/src/lower/stabilize.rs`):

- A counted value is shared into a statement temporary. Any other value is shared (a string
  clone, a copy for plain values) only where the call can replace it, so the callee keeps what
  it was passed, as in JavaScript: any callee for a value in an array's buffer or a module slot,
  a callee that can run a closure for one in a cell, and a callee that can write the field for
  one in a field (`replaceable.rs`: a field written only by its own constructor cannot be
  replaced). Calls through function values, calls of functions that pass the parameter on to a
  function value (`velt_sema::value_params`) and dynamically dispatched methods are treated as
  able to write anything their types allow. A path from a variable no closure assigns, with no
  field on the way written anywhere, is left alone: nothing can replace `o.name` in
  `k(ctx, o.name)`.
- Otherwise the value is read through containers retained for the statement (`retained_hop`),
  which costs no count update on the value. When the array is a boxed local or parameter that no
  cell holds and its count is 1 after the last argument is evaluated, the element is borrowed in
  place.
- Compound assignments form the place again after the right-hand side, and assignments store the
  new value before the old one is dropped.
- `for...of` over a boxed array, or one reached through a counted object, works like
  JavaScript's array iterator (`for_of_shared.rs`): the loop holds a reference to the array,
  re-reads the length every iteration and shares each element into the binding.
- A variable held in a shared cell (`LocalDef::boxed`, shared with the escaping closures that
  capture it) is like a counted container: by-reference `const`s, destructured bindings and
  `for...of` loops over a place rooted in it are stabilized the same way.
- A module state slot (a module-level `let`, or a module `const` evaluated once) is a cell every
  function reaches. A place inside it is stabilized, and so is the slot itself when a call may
  change what it holds: when some code reassigns it, or when it holds an array (any function may
  empty or grow it) or a struct, class, object or tuple with an owning part. Such a type is
  counted. Maps re-read their entries every step of a loop and pass shares to callbacks, so a
  `const seen = new Map<string, number>()` nothing reassigns stays uncounted.
- A method's receiver is kept alive for the call when the method may replace the place it is
  read from (`receiver.rs`): `cur.bump()` where `bump` calls a function that assigns
  `cur = new State()`. The function value of a call through a function value is kept alive the
  same way when it is read from such a place, since the function may replace its holder
  (`this.f = …`) and so drop its own environment while it runs.

Every stabilized borrow is a fact: a container that becomes counted makes the values borrowed
through it counted too, so uncounted values stay reachable only through unique owners.

### `noalias`, `readonly` and exclusivity

`noalias` (and, for borrowed parameters, `readonly`) is set only when `Cx::unique_refs` holds:
the type is not counted and never borrowed inside a counted object. `velt_opt`'s promotions key
off these attributes ([vir.rs](../../../crates/velt_vir/src/vir.rs) invariant 9). The static
exclusivity errors stay (`append(xs, xs)` is rejected, a deviation from JavaScript); aliasing
created through shares (`const ys = xs; append(xs, ys)`) is correct at run time because those
types carry no `noalias` and interior borrows are stabilized.

### Extern ABI

The runtime sees unboxed layouts (`crates/velt_vir/src/lower/foreign.rs`). A top-level boxed
argument is passed as its pointer (`&T`); an argument or result whose layout contains boxed parts
crosses as an unboxed view (arguments) or is boxed after the call (results, `adopt_foreign`).
A synchronous extern given a boxed array whose elements need no conversion gets the box's own
header, so an extern that replaces the buffer (an out parameter such as
`velt_rt_regex_exec`'s offsets) changes the array itself. Arrays of boxed elements in extern
signatures are not supported.

### Closures and captured variables

Function values share their environment (counted heap environments; `EnvDrop` releases one
reference). A variable that an **escaping** closure captures by value, and that it or the
enclosing function assigns while another party still sees it, lives in a **cell**: a counted
block holding the value (`LocalDef::boxed`, set by `crates/velt_sema/src/ownership/cells.rs`
from the move dataflow; `crates/velt_vir/src/lower/cells.rs`). The enclosing function and every
capturing closure reach it through the cell; reassignment drops the old value in place. A closure
that is the only remaining user (`makeCounter`) keeps a plain copy. A local async closure
(`ownership/local_async`) never leaves its task: a variable it assigns, or one assigned after the
capture, lives in a cell, and each call shares its captured objects. Variables captured by async
closures that may run on other threads keep their own copies: assigning one after the capture is
an error.

### Threads

Counts are not atomic, so no counted object may be reachable from two threads. A call of an
async closure is a started promise on the caller's task, never a thread boundary. Code runs on
another thread only in a task created, or a value handed over, at one of these places, each of
which goes through the transfer glue (`crates/velt_vir/src/lower/transfer.rs`,
`lower/glue/transfer.rs`):

- `spawn(...)` and `spawn(..., { worker })` (`Intrinsic::Spawn`, `SpawnHandled`), a task scope's
  `spawn`, and a spawn inside a library the program hands a callback to (`serve`'s handler): the
  spawned promise's callee, arguments and result;
- a channel send, and a value that settles a promise another task awaits (`Intrinsic::Transfer`);
- `shared(...)`, `new Mutex(...)` and `new RwLock(...)`, whose value every thread may reach, and
  an HTTP handler's environment;
- a native thread's entry.

A transferred value is moved in place when its count is 1 (its parts transferred in turn) and
deep-copied for the task when still shared (structured clone at a worker boundary), the
sender's reference released; values whose type cannot reach a counted object move untouched
(`Intrinsic::NeedsTransfer` is a constant for them, so plain replies settle at once). Class
objects in a hierarchy and interface values transfer through a vtable slot (`SLOT_TRANSFER`),
closures through their environment's transfer entry: a shared environment is cloned first, and a
captured variable's cell that the creator still shares is copied, so the task's assignments stay
in the task. A promise is marked (`velt_rt_fut_transfer`) and its result is transferred by the
task that produces it. A value settled on a promise from another task is transferred by a promise
that task drives, after the settling statement released its own reference
(`std/prelude/promise.velt`). Timers run their callback as a promise of the task that created
them (`std/prelude/timers.velt`). A std API that keeps a callback and runs it on another thread
reaches it through one of the places above, never through a runtime hook of its own.

- **Module state** is not a boundary: a closure stored in a module-level variable is called by
  tasks on several workers and shares its captured cells with the task that stored it, as in
  Node. Every task that reaches module state holds the module lock for the whole of each poll
  from its first access on (`crates/velt_rt/src/task/local/module_state.rs`), so those uses never
  overlap; the debug runtime's cell check (`cell_owner.rs`) aborts on a use of a guarded cell
  without the lock.
- **Which closures cross.** A closure passed to an unknown callee (a function value, an interface
  method, an overridden method) crosses when a value that crosses holds its function type
  (`ownership/local_async/types.rs`). A callback that spawns its argument is followed per call
  site (`crossing_params`), and a crossing value whose type mentions a type parameter crosses
  with that parameter's instantiations, so one spawn helper does not make every closure of its
  function type cross. Where the analysis does not follow a path, the transfer at the boundary
  still copies what is shared, and the many-threads check (`lower/glue/many.rs`) panics on the
  rest; a captured variable's cell that crosses through a spawn of a function value is guarded
  only by the analysis.
- **Changes lost to a copy are errors.** A sync closure that changes what it captured and is sent
  to another task on a channel would run on a copy, so it is an error, as for an async closure,
  and a library's spawn is checked the same way. A closure the program spawns itself runs on its
  own copy and may change it.
- **Async closures** that may reach another thread copy what they capture per call (deep copies
  of shared captures), since an HTTP handler runs them concurrently. A capture owning a resource
  without `clone()` is shared with the call instead; such a capture still referenced by the
  caller panics in the transfer.
- **`shared<F>` of a function type** is called directly (`Intrinsic::SharedRead`), without a
  lock, and each call runs on its own copy of the captures (`lower/callee.rs`
  `shared_call_env`, `glue/transfer_env.rs` `answer_call_copy`), keeping cycles and identity
  within the copy. Sema refuses closures it follows into `shared(...)` that change their
  captures or capture, by value, a lock, an `Atomic` or a class whose `clone()` may change its
  object (`ownership/many.rs`); where sema did not follow, `shared(x)` runs the many-threads
  check's strict walk. A `Mutex` around the function runs the calls one at a time on its own
  captures and allows all of these. A lock shared by value (`const q = m`) is an error.
- **`Mutex.with`** is a boundary too: its result is transferred while the lock is held
  (`async_fn/sync.rs` `leave_lock`), and sema (`ownership/locked`) transfers what the callback
  stores between the protected value and its captured state, following per-function summaries
  of direct calls. A promise made from the value is given copies when it only reads them and is
  rejected otherwise, since it would run after the lock is released.
- **Resources.** A deep copy of a class with its own `clone()` calls it when a field-by-field copy
  would duplicate a resource (a `[Symbol.dispose]` hook of its own or of a part, or a promise);
  the copy is then transferred in turn (`settle_copy`). A resource without `clone()` cannot be
  copied: sema rejects a `spawn` argument or spawned capture that is still used afterwards, and
  one passed through a function value, vtable or interface (`ownership/boundary.rs`); a copy
  that only turns out to be needed at run time panics (`lower/glue/clone.rs`).
- A caught panic ends the transfers it unwound out of (`crates/velt_rt/src/transfer_map.rs`).

## Runtime cost

- Hot code (benchmarks, inner loops) works on uniquely owned data and compiles as unique code;
  a program that shares pays only for the types it shares (`VELT_DEBUG_COUNTED=1` prints the
  counted types of a build).
- Count operations appear only where JavaScript semantics require sharing; counts are
  non-atomic except for strings and `shared<T>`; numbers and Copy structs are never counted;
  small strings never touch the heap.
- **Gate.** A change to the memory model passes, on every primary platform: all end-to-end
  tests in debug and release; `bench/compare.ps1` and `bench/benchmarks-game/compare.ps1`
  (LLVM release, interleaved), with every benchmark within ±3% of the base measured in the same
  session; the HTTP benchmark (`bench/http/compare.ps1`) not worse in requests per second or
  peak memory beyond noise; and the count statistics of `bench/rc_stats.ps1`
  (`VELT_RC_STATS=1`, debug runtime), where hot loops show no count operations.
- `// check: no leaks` goldens (`tests/golden/lang/share_*.velt` and others) assert
  `blocks=A/F` with A = F: the runtime counts every `velt_rt_alloc` / `velt_rt_free` block.

## Diagnostics

- "floating promise: this promise is neither awaited nor spawned", with the note that its result
  and errors would be lost and the fixes `await …` or `spawn(…)`.
- `` cannot copy a `T` for another thread (`spawn`, a channel, `shared(...)` or across a
  `Mutex.with` lock): other references to it are still in use, and … `` for a resource without
  `clone()` that is still shared at a boundary.

## Compatibility

Valid TypeScript that shares objects, arrays, maps and closures behaves as in Node; the
`tests/golden/lang/share_*.velt` programs are checked against Node. The differences:

- the static exclusivity errors (`append(xs, xs)`);
- `struct` values have no identity;
- `using x` of a value that is also shared elsewhere disposes at the last reference, not at the
  block end, and an explicit `x[Symbol.dispose]()` of a shared object releases that reference
  only;
- deep copies at `spawn` and handler boundaries do not preserve aliasing inside the copied graph;
- `Map` and `Set` keys of tuple type compare part by part (JavaScript compares arrays by
  identity).

## Alternatives considered

**A tracing garbage collector.** It gives JavaScript's semantics directly, but brings heap
scanning, pauses or concurrent barriers, and heap-size tuning to every program, including the
ones that never share a value. Counting only the shared types keeps unique code free.

**Rust ownership exposed to users.** Moves, borrows and `.clone()` in source code are the main
porting cost from TypeScript and a source of errors TypeScript developers do not expect.

## Limitations

- Counting is per type, not per allocation site or field.
- No Perceus-style reuse of a dropped unique value's memory, and no elision of retain/release
  pairs around stabilized calls.
- A promise created outside any task (synchronous `main`) is lazy; spawning it runs it on the new
  task with its arguments as they were passed, not transferred, so the caller must not use them
  afterwards.
- `Mutex.with` callbacks and `attempt(f)` are not stabilized like ordinary calls. A `Mutex.with`
  callback that sema cannot resolve to closures or named functions
  (`ownership/locked/values.rs`) is an error when the value can hold objects, and calls through
  virtual and interface methods are assumed to store any argument into any argument they modify.

## Not yet implemented

- **Freeing cyclic garbage** ([cycles.md](cycles.md)): a collector confined to cycle-capable
  types, run synchronously per task by trial deletion, so programs that never write `weak` (as
  ported TypeScript never does) free their cycles as in Node; acyclic garbage is still freed at
  its last reference.
- **`weak T` fields** (`parent: weak Node | null`), which take a type out of the collector's set,
  and a `hint`-level lint on cycle-capable types with a fix-it to `weak`.
