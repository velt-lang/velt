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
//!     - `Copy` (Copy structs and tuples): pointer to a caller-made copy; nobody drops it. An
//!       argument that nothing can change while the function runs (a variable never assigned,
//!       closures and nested functions included, that is not a cell or an alias of an element,
//!       or such a `Copy` param) is passed by its own address instead (lower/call.rs). The callee
//!       never writes through a `Copy` param's pointer (one it assigns is copied on entry), so
//!       the param is `readonly`. A `struct` value (#641) of at most 16 bytes, without padding
//!       between its numbers, passed to a named function (not a method, which a vtable or an
//!       interface table may call; not a closure; not a generated body) travels in registers
//!       instead: as its scalar fields, each its own param, when it has at most four and all
//!       are numbers or booleans (through nested structs), else as the `V128` of its bytes. Not
//!       on WebAssembly. The callee writes them to a slot of its frame and reads the param
//!       through a pointer to it (lower/reg_args.rs).
//!   - Inside the callee, an aggregate param local has VIR type `Ptr`; the value is accessed through
//!     `Place { local, proj: [Proj::Deref(Agg(..))] }`.
//!   - Class instances (`Ptr`) are scalars: `Borrow`/`BorrowMut` pass the object pointer; `Owned`
//!     moves the object (the callee drops it).
//! * **Return value**:
//!   - scalar → the VIR return type;
//!   - `void`/`never` → `Ty::Unit`;
//!   - a `struct` value of at most 16 bytes without padding between its numbers (not on
//!     WebAssembly, not from a throwing function) → the `V128` of its bytes, through a 16-byte
//!     slot on each side (lower/abi.rs `RegRet`); a padded one (`{ ok: boolean; v: f64 }`) is an
//!     aggregate, below: once inlined, LLVM splits its slot into fields only that way;
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
//!   at byte `8 * (k + 9)`; slots -1 to -6 are drop/clone/format/share/class name/transfer of
//!   the concrete value, -7 and -8 a `struct` value's content hash and equality (its interface
//!   tables only), and the first word (-9) is its class id for `instanceof`
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
//! promises) are polled through their header (`f->poll(f, cx)`, inline). `spawn` of a direct
//! call or an async closure literal copies its initial state into the task (`velt_rt_spawn`).
//! `await Promise.all([…])` of an array literal polls its children in place (done flags in the
//! state); any other `Promise.all(ps)` wraps `velt_rt_all[_with_drop]` in a small compiled
//! future whose result is the `T[]`. Each call of an async closure clones its owned captures
//! into the new state.
//! `async main`: `velt_main` builds the state on its stack and calls `velt_rt_block_on`.
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
//! # JSON (`lower/json.rs`)
//! `JSON.parse<T>` and `JSON.stringify` are Velt code source generators write per type
//! (velt/encoding/json/generate); lowering only prints a `JsonValue` (from the runtime tree its
//! `[JSON_RUNTIME_HANDLE]()` makes) and walks a `Record`'s entries in key order.

pub mod vir;

/// The name of the aggregate every array is (`{ data: Ptr, len: U64, cap: U64 }`), which no
/// user type can have: optimizations may rely on an array's `len` (field 1) being an element
/// count, at most [`MAX_ARRAY_LEN`].
pub const ARRAY_AGG_NAME: &str = "T[]";

/// The most elements an array holds, and the most bytes its buffer holds: 2^52 - 1 (JS allows
/// 2^32 - 1 elements; Velt goes further, rt_abi.md "Array length limit"). Generated code checks
/// it where it chooses a length (`lower/array`), `velt_rt_realloc` where an array grows, and the
/// runtime where it hands a Rust `Vec` over (`velt_rt` `bytes::MAX_ARRAY_LEN`), so `numrep` can
/// rely on it: an index that runs to `len` inclusive stays below 2^53. Far beyond any memory:
/// 2^52 bytes are 4 PiB.
pub const MAX_ARRAY_LEN: u64 = (1 << 52) - 1;

/// The panic message of an array that would pass [`MAX_ARRAY_LEN`] (elements or bytes).
pub const ARRAY_LEN_MESSAGE: &str =
    "RangeError: Invalid array length: an array holds at most 2^52 - 1 elements, and its elements at most 2^52 - 1 bytes";

/// The name of the one-byte aggregate (`{ byte: U8 }`) that lowering reads a string's UTF-8
/// bytes through (`(*p as string text).0`, `strByteAt` and `charCodeAt`'s ASCII
/// path), which no user type can have: the LLVM backend tags those loads as array element
/// reads for alias analysis (velt_codegen_llvm `tbaa.rs`, #1669). A string's text lives in its
/// own buffer or inline in the string value, never in a class object's scalar field or an
/// array header.
pub const STR_TEXT_AGG_NAME: &str = "string text";

/// The end of the name of every class object's aggregate (`<class> object`), which no other
/// aggregate's name has: the LLVM backend tags the scalar fields of such objects for alias
/// analysis (velt_codegen_llvm `tbaa.rs`).
pub const OBJECT_AGG_SUFFIX: &str = " object";

pub mod atomic;
pub mod eh;
pub mod fingerprint;
mod lower;

/// The argument types of lowering's typed hook builders (`lower/hook_calls.rs`), generated from
/// velt_common's hook table: public only so that this doctest can show a call with the wrong
/// types does not compile. `intPush(buf, v)` takes the builder, then the integer:
///
/// ```
/// use velt_vir::hook_args::{int_push, ty, HookTy};
/// use velt_vir::vir::{Const, Operand, Ty};
/// let op = || Operand::Const(Const::Int(0), Ty::I64);
/// let _: int_push::Args = (ty::Str::arg(op()), ty::I64::arg(op()));
/// ```
///
/// Swapped, or with a float, it is a type error:
///
/// ```compile_fail,E0308
/// use velt_vir::hook_args::{int_push, ty, HookTy};
/// use velt_vir::vir::{Const, Operand, Ty};
/// let op = || Operand::Const(Const::Int(0), Ty::I64);
/// let _: int_push::Args = (ty::I64::arg(op()), ty::Str::arg(op()));
/// ```
///
/// ```compile_fail,E0308
/// use velt_vir::hook_args::{int_push, ty, HookTy};
/// use velt_vir::vir::{Const, Operand, Ty};
/// let op = || Operand::Const(Const::Int(0), Ty::I64);
/// let _: int_push::Args = (ty::Str::arg(op()), ty::F64::arg(op()));
/// ```
#[doc(hidden)]
pub mod hook_args {
    pub use crate::lower::hook_calls::args::*;
    pub use crate::lower::hook_calls::{ty, HookTy};
}
pub mod mangle;
pub mod math;
pub mod parallel;
pub mod simd;
pub mod system;
pub mod tls;
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
    /// Debug builds against a debug runtime: report the creation, count changes and transfers
    /// of captured variables' cells, and calls of closures assigning them, to the runtime
    /// (`velt_rt_cell_*`, rt_abi.md "Counted objects"), which aborts when two tasks use one
    /// cell (#916). Off, the generated code is unchanged.
    pub cell_checks: bool,
    /// Describe source variables for debuggers (`Program::debug_types`, `LocalDecl::debug`;
    /// vir/debug.rs). Needs `source_map`. Off, the program is the same without them.
    pub debug_info: bool,
    /// The target triple: system functions (`declare system function`, [`system`]) are bound
    /// per target. `None`: the host's.
    pub target: Option<&'a str>,
    /// Share generic instances by shape (docs/internals/design/shape-sharing.md): debug builds.
    /// `VELT_SHAPES=off` / `all` overrides it.
    pub shapes: bool,
    /// The code generator has no CPU extension's instructions (Cranelift): every
    /// `velt_simd_has_<ext>` is false in this build, so what only such a check reaches is not
    /// lowered (lower/cpu_checks.rs). `false` leaves only the target's architecture to decide.
    pub no_cpu_extensions: bool,
    /// The counted types of the previous version of this program ([`lower_seeded`], `velt dev`):
    /// the first pass starts from them, so it is often the only one.
    pub counted_seed: Option<&'a CountedSeed>,
}

pub use lower::CountedSeed;

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
    lower_with(program, &LowerOptions::default())
}

/// CONTRACT: [`lower`] with options (source locations for panics and debug info). Panics
/// (`ICE:`) where [`lower_checked`] reports an error.
pub fn lower_with(program: &hir::Program, opts: &LowerOptions) -> vir::Program {
    match lower_checked(program, opts) {
        Ok(p) => p,
        Err(diags) => panic!("ICE: {}", diags[0].message),
    }
}

/// CONTRACT: [`lower_with`], reporting what a checked program can still ask for that lowering
/// cannot represent yet: an instance that would hold both `null` and `undefined` in one value
/// (issue #930; sema reports the cases it can see, `undefined_args.rs`).
pub fn lower_checked(
    program: &hir::Program,
    opts: &LowerOptions,
) -> Result<vir::Program, Vec<velt_common::Diagnostic>> {
    let (p, diags) = lower::lower_program(program, opts);
    if diags.is_empty() {
        Ok(p)
    } else {
        Err(diags)
    }
}

/// [`lower_checked`], also returning the program's counted types as a seed for lowering its next
/// version (`LowerOptions::counted_seed`). For `velt dev`: a seed that names a type the next
/// version no longer needs keeps it counted (lower/boxing/seed.rs), so the next version's code
/// may differ from a lowering without the seed in representation (more counting), not in
/// behavior: what would change behavior (an `RwLock`'s exclusive `read`) is never seeded.
pub fn lower_seeded(
    program: &hir::Program,
    opts: &LowerOptions,
) -> Result<(vir::Program, CountedSeed), Vec<velt_common::Diagnostic>> {
    let (p, diags, seed) = lower::lower_program_seeded(program, opts, true);
    match diags.is_empty() {
        true => Ok((p, seed.unwrap_or_default())),
        false => Err(diags),
    }
}

/// CONTRACT: check VIR invariants (see vir.rs header). Returns human-readable errors.
pub fn verify(program: &vir::Program) -> Result<(), Vec<String>> {
    verify::verify_program(program)
}
