//! HIR → VIR lowering (monomorphization, drop elaboration, async state machines) + VIR verifier.
//! `vir.rs` and the `lower`/`verify` signatures are contracts (maintainer-owned).
//!
//! # Calling convention of lowered Velt functions
//!
//! VIR signatures are scalar-only (vir.rs invariant 2). A HIR `FnDef` becomes a VIR `Function` with:
//!
//! * **Params, in HIR order** (Unit-typed params are omitted entirely):
//!   - scalar type (ints, floats, bool), any `PassMode` → passed by value as that scalar type
//!     (`isize`/`usize` are `I64`/`U64`);
//!   - aggregate type (`string`, structs, arrays, … — see "Layouts") → one `Ptr` param:
//!     - `Borrow` / `BorrowMut`: pointer to the caller's value. The caller keeps ownership; the
//!       callee never drops it (a callee that assigns a new value — a `Mutex.with` callback —
//!       drops the old one in place, like `*p = v` in Rust).
//!     - `Owned`: pointer to a caller-made bitwise copy in a caller stack temporary. Ownership moves
//!       to the callee, which drops it through the pointer at its scope exit (unless moved on); the
//!       caller never touches that temporary again.
//!     - `Copy` (M2 Copy structs): pointer to a caller-made copy; nobody drops it.
//!   - Inside the callee, an aggregate param local has VIR type `Ptr`; the value is accessed through
//!     `Place { local, proj: [Proj::Deref(Agg(..))] }`.
//!   - Class instances (`Ptr`) are scalars: `Borrow`/`BorrowMut` pass the object pointer; `Owned`
//!     moves the object (the callee drops it).
//! * **Return value**:
//!   - scalar → the VIR return type;
//!   - `void`/`never` → `Ty::Unit`;
//!   - aggregate → an extra **trailing** `Ptr` param ("out pointer") to uninitialized caller memory;
//!     the callee writes the (owned) result there and returns `Unit`. The caller then owns it.
//!   - throwing function (`FnDef::throws`) → always an out-pointer to a `Result<ret, E>` aggregate
//!     (tag `u8` at offset 0: 0 = Ok, 1 = Err; payload in the variant view). Callers test the tag
//!     and route an error to the innermost `try` handler or propagate it (no unwinding).
//! * **Closures** (functions with captures) take a hidden **first** `Ptr` param: the environment.
//! * **Generic functions** are monomorphized: one VIR function per (def, concrete type args), symbol
//!   `<mangled name>_T<tyid>_<tyid>…`; only functions reachable from `main` are lowered.
//! * **Dynamic calls** (function values, virtual methods, interface methods) use the *borrow ABI*:
//!   `(first: ptr, args…, [out])`, every aggregate argument by pointer and never owned by the
//!   callee. `first` is the closure env / receiver data pointer. Callees that need adapting (named
//!   functions as values, owned params, scalar receivers) are reached through thunks.
//! * **Symbols**: user functions are `Linkage::Internal` with a mangled symbol (see [`mangle`]:
//!   `_V` + length-prefixed escaped segments, e.g. `User.greet` → `_V4UserM5greet`).
//!   The exported entry `velt_main() -> I32` calls the user `main` and returns its `i32` result, or 0
//!   for a `void` main.
//! * **`declare function` externs** use the same mapping (aggregates by pointer, aggregate results via
//!   a trailing out pointer); scalars follow the C ABI directly.
//!
//! # Other lowering conventions
//! * Integer `/` and `%` check the divisor for zero (branch to a per-function block that calls
//!   `velt_rt_panic("division by zero")`); signed `x / -1` is lowered as wrapping negation and
//!   `x % -1` as `0`, so backends never see the `MIN / -1` trap. Float `%` is emitted as
//!   `BinOp::Rem` on floats with C `fmod` semantics (codegen calls libm `fmod`/`fmodf`).
//! * `**` calls `velt_rt_pow_i64` / `velt_rt_pow_f64` (operands widened to 64 bits, result cast back).
//! * String comparisons call `velt_rt_str_cmp` and compare its result with 0.
//! * Drops: owned non-Copy locals are dropped at every scope exit unless moved. A local whose
//!   ownership changes inside a conditional region gets a `Bool` drop-flag local (`<name>.dropflag`),
//!   initialized to `false` in the entry block. Owned temporaries (call/concat/to-string results that
//!   are only borrowed) are dropped at the end of the enclosing statement.
//!
//! # Layouts (M2)
//! Natural alignment, fields in declaration order (see `lower/layout.rs`, `lower/types.rs`):
//! * struct / anon object / tuple → inline aggregate; `void` fields (`IoResult<void>`) take no
//!   space and have no VIR field (later fields shift down);
//! * class → `Ptr` to a heap object `[vtable: ptr]? + fields` (base-class fields first; the vtable
//!   pointer exists when the class hierarchy has a subclass or a virtual method). Objects are
//!   allocated with `velt_rt_alloc`, zero-filled, then the constructors and field initializers
//!   run in JavaScript's order (`lower/ctor_init.rs`);
//! * C-like enum → `I64` discriminant; other enums → `{ tag: u32 }` base sized for the largest
//!   `{ tag, payload… }` variant view (tag = variant index);
//! * `T | null` → `Ptr` (null = none) for classes and `shared<T>`; otherwise `{ bool, T }`;
//! * `T[]` → `{ data: ptr, len: u64, cap: u64 }`; `shared<T>` → `Ptr` to `{ count: u64, value }`;
//! * function values → `{ code: ptr, env: ptr }`; the env is
//!   `{ drop: ptr, clone: ptr, transfer: ptr, captures… }` (null for named functions / capture-less
//!   closures; stack-allocated with null drop/clone when the closure only borrows);
//! * interface values → `{ data: ptr, vtable: ptr }` (data = the object for classes, else a heap box).
//! * Vtables are read-only tables of function addresses (static data with relocations): slot `k`
//!   at byte `8 * (k + 7)`; slots -1 to -6 are drop/clone/format/share/class name/transfer of
//!   the concrete value, and the first word (-7) is its class id for `instanceof`
//!   (`lower/glue/vtable.rs`, `lower/class_test.rs`).
//! * Every type's all-zero bit pattern is a valid "owns nothing" value for its drop glue. A
//!   struct/class with a `dispose()` hook runs it before its fields are dropped.
//! * `Promise<T>` values → `Ptr` to a heap future (`VeltFut*`, result at +16).
//!
//! # Async functions (M3, `lower/async_fn/`, rt ABI in `docs/internals/contracts/rt_abi_async.md`)
//! An async function instance `f` becomes three functions:
//! * `f$poll(state: ptr, cx: ptr) -> u32` (0 = pending, 1 = ready): the body lowered once; the
//!   entry block switches on the state's tag (`0` start, `k` resume after suspension `k`,
//!   `0x8000_0000 | k` cancel at suspension `k`, `0x7FFF_FFFF` finished). The result (a
//!   `Result<T, E>` for throwing functions) is written at `state + 0`.
//! * `f$drop(state: ptr)`: sets the cancel bit and runs the poll function, whose cancel blocks
//!   drop the pending child and every live value (never `finally` code, never the result).
//! * `f(args…) -> ptr` with the ordinary calling convention: the promise *value*, i.e. the
//!   initial state boxed with `velt_rt_fut_box` (for async closures: the closure's `code`). A
//!   throwing `f`'s value is wrapped so `+16` holds `T`; an `Err` there is an uncaught error.
//!
//! The state struct is `[result @0] tag: u32 @field 0, then the VIR locals that survive a
//! suspension` — computed after lowering by liveness at resume points plus points-to
//! (`async_fn/spill/`); locals never needed at the same time share bytes (interference, first
//! fit). `await` of a direct call to a compiled async function embeds the child's state in
//! the parent's (no allocation); other promises (rt leaf futures, join handles, boxed
//! promises) are polled with `velt_rt_fut_poll`. `spawn` of a direct call or an async closure
//! literal copies its initial state into the task (`velt_rt_spawn`). `await Promise.all([…])`
//! of an array literal polls its children in place (done flags in the state); any other
//! `Promise.all(ps)` wraps `velt_rt_all[_with_drop]` in a small compiled future whose result
//! is the `T[]`. Each call of an async closure clones its owned captures into the new state.
//! `async main`: `velt_main` builds the state on its stack and calls `velt_rt_block_on`.
//!
//! `__intrinsic_http_handler(async (raw) => …)` (std/http) is the `VeltHandler` 6-tuple
//! `{ init, poll, drop, state_size, state_align, env }`: the closure's state machine is the
//! per-request state, lowered to *borrow* its captures from the environment that concurrent
//! requests share and the runtime releases once the server is closed and its last request
//! finished (`async_fn/handler.rs`).
//!
//! # Source locations (`lower_with`)
//! With a source map, every VIR statement and terminator records the location of the HIR
//! statement/expression it came from (`Function::locs`, `Program::files`; vir.rs invariant 8),
//! and compiler-emitted panics end in ` at <path>:<line>:<col>`: division by zero, index out of
//! bounds (the `Oob` helper takes the suffix as a third argument), `panic(msg)`, and panics
//! inside caller-tracking standard-library functions such as `unwrap()` (which report their
//! call site; `lower/track_caller.rs`). `throw`, `?` in a throwing function and `JSON.parse`
//! failures call `velt_rt_set_throw_loc(" at …")`, and an `Uncaught …` report appends
//! `velt_rt_throw_loc()`. Plain `lower` emits none of this (no locations, empty suffixes).
//!
//! # JSON (M4, `lower/json/`)
//! `JSON.stringify`/`JSON.parse<T>` call one generated function per type (`Glue::JsonWrite` over
//! the rt string builder, `Glue::JsonRead` over the rt pull reader, `Glue::JsonParse` for the
//! document); a failed parse throws the prelude's `JsonError { message }` with a
//! `$.field[3]`-style path built only on the failure path.

pub mod vir;

mod lower;
pub mod mangle;
mod verify;

#[cfg(test)]
mod tests;

use std::path::Path;

use velt_common::SourceMap;
use velt_sema::hir;

/// Options for [`lower_with`].
#[derive(Clone, Copy, Default)]
pub struct LowerOptions<'a> {
    /// Sources of the program: fills `Function::locs` / `Program::files` and the locations in
    /// panic and uncaught-error messages. `None` = no location information (like [`lower`]).
    pub source_map: Option<&'a SourceMap>,
    /// Root of the standard library: its functions that panic on behalf of their caller
    /// (`unwrap`, `assert`, …) report the call site instead of their own location.
    pub std_root: Option<&'a Path>,
    /// Native libraries of packages (docs/internals/contracts/native_abi.md): `velt_main` starts
    /// by calling each one's init function with the runtime's table, in this order.
    pub native_inits: &'a [NativeInit],
}

/// One native library to initialize before `main` runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeInit {
    /// The package (named in the start-up error if its init fails).
    pub package: String,
    /// Its init function, `velt_native_init_<pkg>`: `(const VeltRtApi*) -> i32`.
    pub symbol: String,
}

/// CONTRACT: lower a checked program. Infallible for any `Program` sema accepted
/// (internal bugs may panic with a clear "ICE:" message).
pub fn lower(program: &hir::Program) -> vir::Program {
    lower::lower_program(program, &LowerOptions::default())
}

/// CONTRACT: [`lower`] with options (source locations for panics and debug info).
pub fn lower_with(program: &hir::Program, opts: &LowerOptions) -> vir::Program {
    lower::lower_program(program, opts)
}

/// CONTRACT: check VIR invariants (see vir.rs header). Returns human-readable errors.
pub fn verify(program: &vir::Program) -> Result<(), Vec<String>> {
    verify::verify_program(program)
}
