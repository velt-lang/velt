//! The `Intrinsic` table of the HIR (part of the hir.rs contract): compiler-known operations,
//! split out of `hir/mod.rs` by concern.

/// Compiler-known operations. Lowering maps each to inline code or rt calls (see rt_abi.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intrinsic {
    // M1
    /// `console.log(a, b, ...)`: prints args separated by ' ' then '\n'. Args are Borrow uses.
    /// Arg types in M1: ints, floats, bool, Str.
    Print,
    /// `console.error(...)`: same, to stderr.
    PrintErr,
    /// 1 arg of int/float/bool/Str type → owned `Str` (JS formatting for floats: `1`, `1.5`, `NaN`).
    ToString,
    /// (Str borrow, Str borrow) → new owned Str.
    StrConcat,
    /// `s.length` → usize (byte length in POC).
    StrLen,
    /// `process.exit(code: i32)` → Never.
    Exit,
    /// `panic(msg: string)` → Never.
    Panic,
    // M2. Callable from std/ as `__intrinsic_<snake_case_name>(...)`; some are user-visible too.
    /// `(cap: usize) -> T[]`
    ArrayWithCapacity,
    /// `xs.length` (user-visible) `-> usize`
    ArrayLen,
    /// `xs.push(x)` (user-visible), `x` Owned
    ArrayPush,
    /// `xs.pop()` (user-visible) `-> T | null`
    ArrayPop,
    /// `(xs (modified), i: usize, j: usize)` swap elements, bounds-checked
    ArraySwap,
    /// `(xs (modified), i: usize) -> T` remove + shift left, bounds-checked
    ArrayRemove,
    /// `(xs (modified), len: usize)` drop elements past `len`
    ArrayTruncate,
    /// std only: `(dst (modified), d: usize, src: borrow T[], s: usize, n: usize)` — the
    /// bits of `src[s..s + n)` written to `dst[d..d + n)` (both ranges bounds-checked; `dst`
    /// and `src` are different arrays): what `dst` held there is not dropped, nothing is
    /// shared, and `src` keeps stale copies. A raw move for algorithms that keep every element
    /// owned exactly once themselves (the stable sort's scratch buffer, std/sort/stable.vlt).
    ArrayMove,
    /// std only: `(xs (modified), len: usize)` set the length to `len` (growing the capacity
    /// when it is smaller) without dropping or initializing any element: slots past the old
    /// length must be written (`ArrayMove`) before they are read, and slots that are not owned
    /// any more must be cut off with it before the array is dropped.
    ArraySetLen,
    /// `(x: borrow T) -> u64` compiler-generated hash agreeing with `KeyEq` (`Map` and `Set`
    /// keys): numbers, bool and strings by value, objects by identity, unions, options and
    /// tuples part by part
    Hash,
    /// `(a: borrow T, b: borrow T) -> bool` structural (deep) equality: `__intrinsic_eq`,
    /// `deepEqual`, `assertEq`
    Eq,
    /// `(a: borrow T, b: borrow T) -> bool` JS SameValueZero, the `Map` and `Set` key comparison
    /// (`__intrinsic_key_eq`): `Same` with floats compared so that `NaN` equals itself and `0`
    /// equals `-0`. Objects compare by identity, as `Hash` hashes them.
    KeyEq,
    /// `(a: borrow T, b: borrow T) -> bool` JS `Array.prototype.includes`
    /// (`__intrinsic_includes_eq`): `Same`, except that a number `NaN` equals `NaN` (a number
    /// held by an `any` or an option too). Tuples and the other parts compare as `Same` does.
    IncludesEq,
    /// `(a: borrow T, b: borrow T) -> bool` JS `===` (`==` on non-primitive types): objects —
    /// class instances, arrays, object types, interface and function values — by identity;
    /// `T | null`, unions and tuples part by part; strings and numbers by value
    Same,
    /// `(a: borrow T, b: borrow T) -> bool` JS `Object.is` (SameValue): `Same`, except that
    /// floats compare by value with `NaN` equal to itself and `0` unequal to `-0`
    SameValue,
    /// `x.clone()` (user-visible, every type): deep copy
    Clone,
    /// `(x: borrow T) -> T`: another reference to the same value (JS reference copy; semantics
    /// stage 2, hir_encodings.md "Sharing"): a count increment for counted objects, a copy for
    /// Copy types and strings, a field-wise share for immutable value types. Emitted by sema
    /// wherever a non-Copy place is used by value but stays in use (or cannot be moved from).
    Share,
    /// `(parts: borrow P0, P1, …) -> TyOp::Spread(P0, P1, …)`: an object literal with a spread
    /// whose fields are known only per instantiation (#350). Each part is an object value: a
    /// spread source (a place, borrowed) or a one-field object literal holding an explicit
    /// property (owned). Lowering builds the reduced object type, each field shared from the
    /// last part that has it (an optional field only while present, as JavaScript copies only
    /// the keys an object has).
    SpreadObject,
    /// `(object: borrow T, key: borrow K) -> T[K]` in a generic body (`source[key]` with
    /// `K extends keyof T`, #350): at every instantiation `K` is one key of the object type `T`
    /// (sema checks it), and lowering reads that field, shared, as `object.<key>` would.
    KeyRead,
    /// `(object: borrow mut T, key: borrow K, value: T[K]) -> void`: `object[key] = value` in a
    /// generic body, as for `KeyRead`; lowering writes that field.
    KeyWrite,
    /// `() -> T | null`, only as the value of a `presence` field in an `AdtLit`
    /// (`FieldDef::presence`): the field is absent (its value `null`, its presence flag clear)
    FieldAbsent,
    /// `(field: borrow T | null) -> bool`, `field` a `Field` place of a `presence` field: whether
    /// it is present (written, or given in the literal that made the object)
    FieldPresent,
    /// std only (std/prelude/promise.vlt): `__intrinsic_transfer<T>(value: T) -> T`, the value
    /// made safe for another task (moved where this task held the only reference, deep-copied
    /// where it is still shared; velt_vir transfer.rs), like a `spawn` argument. Also emitted
    /// by sema around a value a `Mutex.with` callback stores across the lock
    /// (ownership/locked).
    Transfer,
    /// std only (std/prelude/promise.vlt): `__intrinsic_needs_transfer<T>(value: borrow T) ->
    /// bool`, a constant: can a `T` reach a counted object, so that `Transfer` has work to do
    /// (the value itself is not read)?
    NeedsTransfer,
    /// std only (std/sort/stable.vlt): `__intrinsic_needs_drop<T>(value: borrow T) -> bool`,
    /// a constant: does a `T` own anything that dropping it releases (strings, arrays,
    /// objects, closures with a heap environment)? A bit copy of a `T` that does not is an
    /// independent value. The value itself is not evaluated.
    NeedsDrop,
    /// std only (std/prelude/array.vlt): `__intrinsic_may_alias<T>(value: borrow T) -> bool`, a
    /// constant: can a value of type `T` that a call borrows be reached through another
    /// reference while the call runs (lowering counts the type, or borrows values of it inside
    /// counted objects)? Only then can a callback change it. The value is not evaluated.
    MayAlias,
    /// std only (std/prelude/map.vlt): `__intrinsic_holds_null<T>(probe: borrow T) -> bool`, a
    /// constant: is `null` a value of `T` (is `T` an option, `X | null`, once substituted)? Then
    /// a generic `T | null` is `T` itself and its `null` is a `T`. The probe is not evaluated.
    HoldsNull,
    /// std only (std/prelude/map.vlt): `__intrinsic_null_as<T>() -> T`, `null` as a `T` that
    /// holds null (`HoldsNull`). For any other `T` it stops the program with an internal error:
    /// std reaches it only behind a `HoldsNull` test.
    NullAs,
    /// Compiler-internal (no source syntax): `(x: borrow T) -> bool`, JS truthiness of a value
    /// whose type is a type parameter (`!x`, `if (x)` on a `T`), the test chosen by lowering
    /// per instantiation as for a value of that type (truthiness.rs in sema): `false`, `null`,
    /// `0`, `-0`, `NaN` and `""` are falsy, a union by its member, objects always truthy.
    Truthy,
    /// Compiler-internal (no source syntax): `(x: borrow T) -> bool`, `x === null` on a value
    /// whose type is an option of a type parameter (`x?: T`, issue #930): whether it holds the
    /// `null` of the type it is instantiated with (the inner nothing of a `X | null | undefined`,
    /// the nothing of a `X | null`); `false` for a type without `null`.
    IsNull,
    /// Compiler-internal: `(x: borrow T) -> bool`, `x == null` on such a value: whether it holds
    /// either nothing once instantiated.
    IsNullish,
    /// Compiler-internal (issue #1251): `(probe: borrow T) -> bool`, a constant: can a value of
    /// the type `T` is instantiated with be a primitive (`false` for a struct, class, object
    /// type, array, tuple, map or function type; `true` for anything else, unions and `any`
    /// included)? The probe is not evaluated. Only as the right operand of `g(x) && _` in the
    /// condition of an `if` whose test `g(x)` is a type predicate with only primitive members
    /// (hir_encodings.md "Type predicates on a type parameter").
    MayHoldPrimitive,
    /// std only (std/sort/stable.vlt): `__intrinsic_fn_captures_nothing<F>(f: borrow F) ->
    /// bool` for a function value: true when its environment is null, which only closures
    /// without captures and named functions have (a program that compares function values
    /// gives every closure an environment, so it is false there). Such a function reaches no
    /// variable of its caller. Lowered to `f.env == null`, which velt_opt folds to a constant
    /// where the function value is known (const_fields after specialization).
    FnCapturesNothing,
    /// Inside a member of a recursive closure group (sema `nested_closures`): the closure
    /// value of member `def` of the same group, made from the running closure's own
    /// environment (`{ code of def, env }`, sharing it). The members capture the same
    /// variables in the same order, so their environments have one layout. No args; the
    /// result is owned.
    ClosureSibling(super::DefId),
    /// In the entry `main` only: evaluate the initializers of module state slots `from..to`
    /// (`ModuleState::deferred` on) into their slots, in order, as the state's initializer does
    /// for the slots before them. No args, `void`. Additive (#1438).
    InitSlots(u32, u32),
    /// f64 math: `Math.sqrt/floor/ceil/round/trunc/abs` (round = JS: half toward +inf)
    Sqrt,
    Floor,
    Ceil,
    Round,
    Trunc,
    FAbs,
    /// `shared(x)` (user-visible) -> `Shared<T>`
    SharedNew,

    // M3 async & concurrency (user-visible builtins; see docs/reference/async.md)
    /// `spawn(p: Promise<T>): Promise<T>` — start now on the runtime, returns a join handle.
    Spawn,
    /// `__intrinsic_spawn_handled(p)` (standard library only): `spawn(p)` whose rejection the
    /// caller handles itself (a `taskScope` child): a dropped handle never reports it.
    SpawnHandled,
    /// `sleep(ms: i64): Promise<void>`
    Sleep,
    /// `yieldNow(): Promise<void>`
    YieldNow,
    /// `Promise.all(ps: Promise<T>[]): Promise<T[]>`
    PromiseAll,
    /// `Promise.race(ps: Promise<T, E>[]): Promise<T, E>` — the first to settle; the others keep
    /// running.
    PromiseRace,
    /// std only: `__intrinsic_promise_any(ps: Promise<T, E>[]): Promise<T, E>` — the first to
    /// fulfill, or the last rejection when all reject (std/prelude/promise.vlt `promiseAny`).
    PromiseAny,
    /// std only (std/channel.vlt): `__intrinsic_chan_send<T>(ch: u64, value: T):
    /// Promise<bool>` — moves `value` into the channel; false (and `value` dropped) if it is
    /// closed.
    ChanSend,
    /// std only: `__intrinsic_chan_receive<T>(ch: u64): Promise<T | null>` — the oldest value,
    /// or null once the channel is closed and drained.
    ChanReceive,
    /// std only: `__intrinsic_chan_try_send<T>(ch: u64, value: T): bool` — moves `value` into
    /// the channel if it has room; false (and `value` dropped) if it is full or closed.
    ChanTrySend,
    /// std only: `__intrinsic_chan_try_receive<T>(ch: u64): T | null` — the oldest value if one
    /// is queued.
    ChanTryReceive,
    /// Compiler-internal (no source syntax): the location of the call's span as a string,
    /// `"path:line:col"` (the site of a `new Promise`, std/prelude/promise.vlt).
    SourceLocation,
    /// Compiler-internal (std: `__intrinsic_symbol_static(id, desc, described)`): `(id: i64
    /// literal, desc: string literal, described: bool literal) -> symbol`, the address of a
    /// read-only symbol record made at compile time (rt_abi.md "Symbols"): one record per `id`
    /// in a program, whatever the call site, so every evaluation gives the same symbol. Ids
    /// below 64 are the well-known symbols (`Symbol.iterator`, std/symbol.vlt), ids from 64 the
    /// module constants initialized with `Symbol()` / `Symbol("d")`, and ids from 2^40 the
    /// registry's symbols of module constants initialized with `Symbol.for("k")` (`desc` is the
    /// key; such a record is listed in [`Intrinsic::SymbolRegistry`]'s table). With `described`
    /// false the symbol has no description (`desc` is `""`).
    SymbolStatic,
    /// Compiler-internal (std: `__intrinsic_symbol_registry()`): `() -> symbol`, the address of
    /// the program's read-only table of the registry's records (the `SymbolStatic` records with
    /// ids from 2^40): their addresses, one per 8-byte slot, ending with a null slot. It is typed
    /// `symbol` only to be passed on as a pointer, to `velt_rt_symbol_for`, whose registry
    /// gives these records for their keys (rt_abi.md "Symbols").
    SymbolRegistry,
    /// Compiler-internal (no source syntax): `ToString` of a template literal part whose type
    /// mentions a type parameter (`${x}` with `x: T`). It is `ToString`, except that where the
    /// value is a symbol once substituted (`symbol`, or the active member of a union or `T | null`
    /// holding one, tested at run time), it stops the program with JS's `TypeError: Cannot
    /// convert a Symbol value to a string`, as JS's `ToString` throws (`String(x)`, a
    /// `ToString`, writes `Symbol(d)`; sema rejects the direct cases).
    TemplateToString,
    /// Compiler-internal (no source syntax): `p: Promise<T, E1>` as a `Promise<T, E2>` whose error
    /// set contains `E1`'s (an implicit conversion, `coerce.rs`): a lazy wrapper that widens
    /// the rejection.
    PromiseWiden,
    /// `performance.now(): f64`
    PerfNow,
    /// `Date.now(): i64`
    DateNow,
    /// `s.add(n)` on `shared<int>` → new value; `s.get()`; `s.set(v)` (atomic)
    SharedAdd,
    SharedGet,
    SharedSet,
    /// Compiler-internal (no source syntax): `s: shared<T>` (no `Mutex`) used as the `T` it
    /// holds, where the value is only read (#36): `s[i]`, `s.length`, a method's receiver, an
    /// argument. Its one argument is borrowed and the result is borrowed out of the shared box:
    /// a borrow of the `T` itself when no part of `T` is reference counted, else (and where the
    /// value is consumed) a copy. Nothing ever writes through it.
    SharedRead,
    /// `new Mutex<T>(x)` → `Mutex<T>` (an ADT-like builtin: lock word + value)
    MutexNew,
    /// `m.with(f)` on `Mutex<T>` or `shared<Mutex<T>>`: lock, call `f(value)` (by reference), unlock, return f's result
    MutexWith,

    // M4 JSON (compile-time generated glue over the rt JSON reader / string builder)
    /// `JSON.stringify(x: borrow T): string`
    JsonStringify,
    /// std only: `__intrinsic_json_parse<T>(text: borrow string, flags: i64, max_depth: i64): T`
    /// — the body of the prelude's `JSON.parse<T>(text, options)`: decodes `text` as `T` with the
    /// reader options of `velt_rt_json_reader_new_with` (flag 1 = reject unknown keys;
    /// `max_depth` 0 = no limit); throws `JsonError { message }`.
    JsonParse,
    /// std only: `__intrinsic_http_handler(f: (raw: u64) => Promise<u64>)` → `[u64; 6]` =
    /// `{init, poll, drop, state_size, state_align, env}` for `velt_rt_http_serve` (the closure's
    /// state machine is the per-request state; `init(env, req, state)` sets `raw = req`; the env
    /// box is leaked and shared by in-flight requests). See std/http.vlt.
    HttpHandler,
    /// std only: `__intrinsic_str_char_code_at(s: borrow string, i: i64) -> i64` — the byte at
    /// `i`, or -1 when out of range (`s.charCodeAt(i)`, POC byte model); inline code, no call.
    StrCharCodeAt,
    /// std only: `__intrinsic_array_data_ptr(xs: borrow T[]) -> u64` — address of element 0.
    ArrayDataPtr,
    /// `attempt(f)`: call `f: () => T throws E` (borrowed) and return its result or error as a
    /// value, of type `T | E` (or `E | null` when `T` is `void`); hir_encodings.md "Errors".
    Attempt,

    // Generators (hir_encodings.md "Generators")
    /// Compiler-internal: `yield v` in a generator body (`v: T` owned, the generator's yield
    /// type) → unit. Suspends the generator with `v` in its value slot. In a `Generator<T, E,
    /// N>` the call has type `N`: the value `next(value)` resumes it with, and its second
    /// argument (owned, evaluated only then) is the value when `next()` passes none.
    Yield,
    /// std only: `__intrinsic_generator_resume<T, E>(g: Generator<T, E> (modified)): bool`,
    /// throws `E` — run the generator to its next `yield` (true: a value is in the slot) or to
    /// its end (false; also every later call). An error thrown by the body is thrown here.
    GeneratorResume,
    /// std only: `__intrinsic_generator_value<T, E>(g: Generator<T, E> (modified)): T` — move
    /// the value of the last `yield` out of the slot (once, after `GeneratorResume` returned true).
    GeneratorValue,
    /// std only: `__intrinsic_generator_return<T, E>(g: Generator<T, E> (modified))` — close
    /// the generator: a suspended body runs its `finally` blocks and disposes its `using`
    /// values as if the `yield` were a `return`; then it is done.
    GeneratorReturn,
    /// std only: `__intrinsic_generator_resume_with<T, E, N>(g: Generator<T, E, N> (modified),
    /// value: N (borrowed)): bool`, throws `E` — `next(value)`: like `GeneratorResume`, and the
    /// paused `yield` evaluates to (a copy of) `value`; a generator that has not started
    /// ignores it.
    GeneratorResumeWith,
    /// std only: `__intrinsic_generator_throw<T, E, N>(g: Generator<T, E, N> (modified), error:
    /// any (borrowed)): u32`, throws `E` — `throw(error)`: resume the body with `error` thrown
    /// at the paused `yield`. 1: the body caught it and yielded again (the value is in the
    /// slot); 0: it caught it and finished; 3: nothing caught it (the generator is closed, as
    /// by `return()`, or it had not started or was finished), and the caller throws it. An
    /// error the body throws (after catching) is thrown here.
    GeneratorThrow,
    /// Compiler-internal: `(g: Generator<T, E, N> (modified)): bool`, throws `E` — one step of
    /// `yield* g` in a generator that takes `next(value)` or may be thrown into (after a first
    /// `GeneratorResume(g)` returned true): yield `g`'s value; then resume `g` the way this
    /// generator is resumed (`next(value)` passes the value on, `throw(e)` throws `e` into `g`
    /// and, if `g` did not catch it, at the `yield*`, closing closes `g`). True when `g` yielded
    /// again.
    YieldDelegate,
    /// Compiler-internal: `(call: owned) -> Generator<T, E>`, the initializer of the hidden
    /// local of a `for...of` over a direct generator call (`for (x of gen(a))`): lowering keeps
    /// the generator's state inline in that local instead of a heap object.
    GeneratorEmbed,

    // Async generators (hir_encodings.md "Async generators")
    /// std only: `__intrinsic_async_generator_resume<T, E>(g: AsyncGenerator<T, E> (modified)):
    /// Promise<bool, E>`, only as the operand of `await` — run the generator to its next
    /// `yield` (true) or its end (false; also every later call), suspending the awaiting
    /// function while the generator awaits. An error thrown by the body rejects it.
    AsyncGeneratorResume,
    /// std only: `__intrinsic_async_generator_value<T, E>(g: AsyncGenerator<T, E> (modified)):
    /// T` — like `GeneratorValue`.
    AsyncGeneratorValue,
    /// std only: `__intrinsic_async_generator_return<T, E>(g: AsyncGenerator<T, E> (modified)):
    /// Promise<void>`, only as the operand of `await` — close the generator: a body suspended
    /// at a `yield` runs its `finally` blocks and disposals (which may `await`) as if the
    /// `yield` were a `return`; one suspended at an `await` is cancelled. Then it is done.
    AsyncGeneratorReturn,
    /// std only: `__intrinsic_async_generator_dispose<T, E>(g: AsyncGenerator<T, E>
    /// (modified))` — close the generator without awaiting (its drop): like
    /// `AsyncGeneratorReturn`, except that a body whose cleanup at that `yield` would `await`
    /// is cancelled instead (drops only, like a cancelled async function).
    AsyncGeneratorDispose,

    // Weak references (std/prelude/weak.vlt; rt_abi.md "Weak references")
    /// std only: `__intrinsic_weakmap_set<K, V>(m: u32, key: borrow K, value: owned V): u32` —
    /// store `value` for the object `key` in weak map `m`, creating the map first when `m` is 0
    /// (with `K`'s and `V`'s glue); returns the map. `K` is counted and weak-capable.
    WeakMapSet,
    /// std only: `__intrinsic_weakmap_get<K>(m: u32, key: borrow K): u64` — the value word
    /// stored for `key` (counted), or 0 when there is none (0 is also a stored null or zero:
    /// `WeakMapHas` tells them apart).
    WeakMapGet,
    /// std only: `__intrinsic_weakmap_has<K>(m: u32, key: borrow K): bool`.
    WeakMapHas,
    /// std only: `__intrinsic_weakmap_delete<K>(m: u32, key: borrow K): bool` — true if there
    /// was an entry (its value is released).
    WeakMapDelete,
    /// std only: `__intrinsic_weakmap_value<V>(w: u64): V` — the value a word `WeakMapGet`
    /// returned (taking over its reference).
    WeakMapValue,
    /// std only: `__intrinsic_weakref_new<T>(target: borrow T): u32` — a `WeakRef` slot of the
    /// object `target`, which is counted and weak-capable.
    WeakRefNew,
    /// std only: `__intrinsic_weakref_deref<T>(r: u32): T | null` — the target (counted), or
    /// null once it was freed.
    WeakRefDeref,

    // The dynamic value `any` / `unknown` (issue #924; hir_encodings.md "any and unknown"). Its
    // operand is a `TyKind::Dyn` of the prelude's `any` or `unknown`, built by `ToDyn` with
    // `Program::any_impl`.
    /// `(x: owned any) -> T` (`Expr::ty`): the value `x` holds, checked: a value of another
    /// type stops the program with a `TypeError` naming the expected and the found type. `T` is
    /// a concrete type, a class (an instance of it or of a subclass), `U | null` (`null` and
    /// `undefined` give `null`) or `any` / `unknown` (the same value).
    AnyTo,
    /// `(x: borrow any) -> string`: JavaScript's `typeof x`.
    AnyTypeof,
    /// `(x: borrow any) -> bool`: JavaScript's truthiness of `x`.
    AnyTruthy,
    /// `(a: borrow any, b: borrow any) -> bool`: `a === b` (numbers by value, `NaN` unequal,
    /// strings by content, objects and functions by identity).
    AnyStrictEq,
    /// `(a: borrow any, b: borrow any) -> bool`: `a == b` (JavaScript's loose equality:
    /// `null == undefined`, `"1" == 1`, `true == 1`).
    AnyLooseEq,
    /// `(x: borrow any, key: borrow string | any) -> any`: `x.key` / `x[key]` (a key of type
    /// `any` holding a number reads like `AnyIndex`): the member of the value held, boxed;
    /// `undefined` when the value's fixed shape has no such member; a `TypeError` for a
    /// `null` / `undefined` receiver or a member not supported through `any` yet. Sets
    /// `Program::dyn_members`.
    AnyGet,
    /// `(x: borrow any, i: f64) -> any`: `x[i]` with a number key (array elements).
    AnyIndex,
    /// `(x: borrow any, key: borrow string, text: string, args: owned any...) -> any`: the
    /// method call `x.key(args)`; each argument is converted to the parameter's type as by
    /// `AnyTo`. `text` names the callee in a `TypeError` (`x.key is not a function`).
    AnyCall,
    /// `(f: borrow any, text: string, args: owned any...) -> any`: calling the function value
    /// `f` holds (`text` as for `AnyCall`).
    AnyApply,
    /// `(x: borrow any, key: borrow string | any, v: owned any) -> void`: `x.key = v` /
    /// `x[key] = v` (a key of type `any` holding a number writes like `AnySetIndex`): `v`
    /// converted to the member's type as by `AnyTo` and stored, or passed to its setter; a
    /// `TypeError` for a `null` / `undefined` receiver, a member the value's fixed shape does not
    /// have (a record takes any key), a getter without a setter, or a member not supported
    /// through `any` yet. Sets `Program::dyn_members`.
    AnySet,
    /// `(x: borrow any, i: f64, v: owned any) -> void`: `x[i] = v` with a number key (an
    /// element of an array, inside its length).
    AnySetIndex,
    /// `(x: borrow any, key: borrow string | any) -> bool`: `key in x` (a key of type `any`
    /// holding a number names an element by its string): whether the value held has the
    /// property (a proxy: its `has` trap); a `TypeError` for a `null` / `undefined` value or one
    /// not supported yet. Sets `Program::dyn_members`.
    AnyHas,
    /// std only: `(x: borrow any, key: borrow string) -> bool`, `delete x[key]` of a value that
    /// is not a proxy (std/proxy.vlt, `Reflect.deleteProperty`): a record's entry is
    /// removed and an optional field made absent (`true`), a key the value does not have is
    /// `true`, an array's `length` is `false`; a `TypeError` for anything else (a fixed field).
    AnyDelete,
    /// std only: `(x: borrow any, op: i64) -> i64`, an array method running on a proxy of the
    /// array `x` holds (velt:proxy/array; identified by its data pointer): `op` 1 starts it, -1
    /// ends it and answers the first hole it leaves (or -1), 2 and -2 start and end a trap call
    /// of it (`velt_rt_proxy_array_call`, rt_abi.md "Array methods on a proxy").
    ProxyArrayCall,
    /// std only: `(x: borrow any, index: i64, op: i64) -> i64`, the holes those methods made in
    /// the array `x` holds (identified by its data pointer): `op` 0 tests element `index`, 1
    /// records it (only in a trap call of a method on `x`, else -1), 2 forgets it, 3 forgets
    /// those from `index` on, 4 gives the first below `index` (or -1)
    /// (`velt_rt_proxy_array_hole`).
    ProxyArrayHole,
    /// `(x: borrow T) -> bool`, made by sema only: whether the value of the class or object type
    /// `T` is a typed proxy (velt_vir's tagged pointer, proxy_typed.rs); `false` for a type that
    /// cannot hold one. A test of a bit, so a plain value keeps its own code path.
    IsTypedProxy,
    /// `(x: borrow any) -> bool`: `Array.isArray(x)` (the value held is an array or a tuple).
    AnyIsArray,
    /// `(x: borrow any) -> string[]`: `Object.keys(x)`, the own enumerable string keys of the
    /// value held in JavaScript's order (an array's or a string's indexes, a record's keys,
    /// the fields of an object); a `TypeError` for `null` / `undefined` or a value not
    /// supported yet. Sets `Program::dyn_members`.
    AnyKeys,
    /// `T.shuffle(a, b, i0, …)` of a SIMD vector type (`velt:simd`): `(a: T, b: T) -> T`, lane
    /// `k` of the result being lane `lanes[k]` of `a`'s lanes followed by `b`'s. The indices
    /// were integer literals in range (sema checks them), so they are part of the operation:
    /// lowering calls the `velt_simd_<T>_shuffle_<i0>_<i1>…` extern (`velt_vir::simd`).
    SimdShuffle(super::SimdTy, [u8; 16]),
}
