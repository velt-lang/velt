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
    /// `(x: borrow T) -> u64` compiler-generated hash (ints, bool, string, Copy structs, enums)
    Hash,
    /// `(a: borrow T, b: borrow T) -> bool` structural (deep) equality: `__intrinsic_eq`, `Map`
    /// keys, `deepEqual`, `assertEq`
    Eq,
    /// `(a: borrow T, b: borrow T) -> bool` JS `===` (`==` on non-primitive types): objects —
    /// class instances, arrays, object types, interface and function values — by identity;
    /// `T | null`, unions and tuples part by part; strings and numbers by value
    Same,
    /// `x.clone()` (user-visible, every type): deep copy
    Clone,
    /// `(x: borrow T) -> T`: another reference to the same value (JS reference copy; semantics
    /// stage 2, hir_encodings.md "Sharing"): a count increment for counted objects, a copy for
    /// Copy types and strings, a field-wise share for immutable value types. Emitted by sema
    /// wherever a non-Copy place is used by value but stays in use (or cannot be moved from).
    Share,
    /// std only (std/prelude/promise.vlt): `__intrinsic_transfer<T>(value: T) -> T`, the value
    /// made safe for another task (moved where this task held the only reference, deep-copied
    /// where it is still shared; velt_vir transfer.rs), like a `spawn` argument
    Transfer,
    /// std only (std/prelude/promise.vlt): `__intrinsic_needs_transfer<T>(value: borrow T) ->
    /// bool`, a constant: can a `T` reach a counted object, so that `Transfer` has work to do
    /// (the value itself is not read)?
    NeedsTransfer,
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
}
