//! VIR: Velt low-level IR. MIR-like control-flow graph over typed locals.
//! Produced by `velt_vir::lower`, consumed by backends (`velt_codegen_cl`, later LLVM).
//! CONTRACT FILE — maintainer-owned (see docs/internals/contracts/README.md).
//!
//! Invariants that lowering guarantees and backends rely on:
//! 1. Fully monomorphic and layout-resolved. Target is 64-bit (`Ptr` = 8 bytes, little endian).
//! 2. **Function signatures are scalar-only**: params/returns are never `Ty::Agg`. Lowering passes
//!    aggregates by pointer (`Ptr`) and returns them via an explicit out-pointer param.
//!    `Ty::Unit` params never appear; a `Unit` return means "returns nothing".
//! 3. Drops, drop flags, moves, bounds checks, overflow policy, async state machines are all already
//!    expanded into ordinary statements/calls. Backends do no semantic work.
//! 4. Block 0 is the entry block (an `Import`, invariant 11, may have none). Locals
//!    `0..params.len()` hold the incoming params.
//! 5. A program's entry is an exported `velt_main() -> I32` (rt_abi.md); code compiled on its
//!    own to link into one (the std cache) declares it (`Import`). Backends need no special case.
//! 7. `BinOp::Rem` on floats is C `fmod` (JS `%`). `Rvalue::Cast` also allows int → `Bool` (`!= 0`).
//!    Lowering guards signed `MIN / -1` (wrapping neg; `MIN % -1` = 0), so backends never trap on it.
//! 6. Every block ends in exactly one terminator; every local is assigned before it is read
//!    (checked by `verify`).
//! 8. Source locations (additive, optional): `Function::locs` is either empty (no information)
//!    or has one entry per block, each with one `Option<SrcLoc>` per statement plus a final one
//!    for the terminator (checked by `verify`). `SrcLoc::file` indexes `Program::files`
//!    (source paths by `FileId`). Only `velt_vir::lower_with` with a source map fills them;
//!    `velt_opt` passes move locations with their statements (inlined code keeps the callee's).
//!    Backends use them for debug info only; panic messages already carry their location.
//! 9. Parameter attributes (additive, optional): `Function::param_attrs` is either empty (no
//!    information) or has one [`ParamAttrs`] per param (checked by `verify`). Attributes are only
//!    set on `Ptr` params and only where sema's exclusivity guarantees hold (https://veltlang.com/docs/reference/memory/
//!    \"Exclusive access"): during a call, memory reachable through a `BorrowMut` param is
//!    reachable through no other param, and `Borrow` params never alias a `BorrowMut`/`Owned`
//!    one. Lowering sets them for user functions from `PassMode` (`BorrowMut` aggregate or
//!    class object → `noalias`, only when values of the type have no other owners: not counted
//!    and never borrowed inside a counted object, docs/design/semantics-stage2.md §3.4;
//!    `Borrow` aggregate or class object → `readonly` unless the value holds a `Mutex` or an
//!    `Atomic` in its own memory, or the
//!    param is written (`LocalDef::mutable`), and `frozen` under the same conditions except
//!    that an `Atomic` is allowed (only its words change); neither `noalias` nor `frozen` when
//!    module state can reach values of the type (a module `const` / `let`, a `static` field:
//!    a callee may write the argument through that variable); aggregate-return out-pointer →
//!    `noalias`;
//!    `nonnull` + `dereferenceable(size)` on all of them; the frame of a poll function
//!    (`Function::is_poll`) → `nonnull` + `dereferenceable(frame size)`, plus `noalias` from
//!    `velt_opt` when no pointer into the frame ever leaves the function). Every caller must
//!    uphold them: `velt_opt` keeps them on clones and specializations and drops a param's
//!    attributes when it rewrites that param. Backends may use them (LLVM param attributes) or
//!    ignore them.
//! 10. Debug variables (additive, optional; vir/debug.rs): `Program::debug_types` is empty and
//!     every `LocalDecl::debug` is `None` unless `LowerOptions::debug_info` asked for them. Each
//!     `LocalDebug::ty` and every debug type's own references index `debug_types`, and every
//!     `AggId` and field index in them names an existing aggregate field (checked by `verify`).
//!     `velt_opt` passes keep a local's description on the local holding the variable's value
//!     and drop it where no single local does.
//! 11. `Linkage::Import` (additive; contracts README): defined elsewhere, never emitted; passes treat it as `Export`.
//! 12. Intrinsics (additive, #1981): `Rvalue::Intrinsic(op, operands)` is a pure operation the
//!     compiler knows, with the operand and result types of `Intrinsic::signature`; for
//!     `Intrinsic::Math(op)` the program declares `op`'s runtime function (`velt_vir::math`),
//!     which backends call where they have no inline code for it (both checked by `verify`);
//!     a `velt_math_*` operation (`MathOp::has_runtime_function` false) has none, and every
//!     backend emits it inline. It reads only its operands and never touches memory, panics or
//!     unwinds. A math operation is never a direct call of its runtime function (checked by
//!     `verify`; an indirect call through its address, a function value, is an ordinary call).
//! 13. Static symbols (additive, #2189): `StaticData::symbol` is `None` (the static is private
//!     to the program's objects) unless `linkage` is `Export` (defined here, visible to the
//!     link's other objects under the symbol) or `Import` (defined by another object of the link
//!     under the symbol; its `bytes`, `relocs` and `mutable` are its contents there, which code
//!     may read, as LLVM's `available_externally`, but are never emitted). Symbols are unique
//!     among the program's statics, functions and externs. An `Import` static's relocations
//!     name only `Export`/`Import` functions and statics, and externs (checked by `verify`).
//!     A C variable (`StaticData::c_global`) is an `Import` static whose contents no pass knows.
//! 14. Unwind edges (additive, #2667; docs/internals/design/unwinding.md "Cleanup in unwound
//!     frames"): `Terminator::Call::unwind` is `Some(pad)` only in programs that catch Velt
//!     panics, on calls of functions and function values (`eh::may_unwind`). When the callee
//!     unwinds with a Velt exception, control continues at `pad` without writing `dest`; the pad
//!     drops what the frame owns and ends in a call of `velt_eh_resume` (`eh::RESUME`, a
//!     backend-defined noreturn extern), which continues that unwind and never has an unwind
//!     edge itself (checked by `verify`). The edge is a CFG successor like `next`. Backends
//!     without unwinding treat it as absent.

use std::fmt;

mod debug;
mod host;
mod intrinsic;
mod print;
pub use debug::*;
pub use host::{HostImport, HostValue, MANIFEST_SECTION};
pub use intrinsic::Intrinsic;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FuncId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExternId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StaticId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AggId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Local(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ty {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    /// 1 byte in memory; 0 = false, 1 = true.
    Bool,
    /// 64-bit untyped pointer.
    Ptr,
    /// 128-bit SIMD vector: 16 bytes whose lanes are given by each operation, not by the type
    /// (as WebAssembly's `v128`). 8-byte aligned in memory, so backends load and store it
    /// unaligned. Only moved, passed, returned, loaded and stored; every operation on it is a
    /// call of a `velt_simd_*` extern that backends emit inline (rt_abi.md "SIMD"). Its only
    /// constant is `Const::Int(0)`, all bits zero.
    V128,
    /// Zero-sized. Only valid as a function return type or the type of a dummy call destination.
    Unit,
    /// Aggregate stored in memory with the given layout. Never in signatures.
    Agg(AggId),
}

impl Ty {
    pub fn is_int(self) -> bool {
        matches!(
            self,
            Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64
        )
    }
    pub fn is_signed(self) -> bool {
        matches!(self, Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64)
    }
    pub fn is_float(self) -> bool {
        matches!(self, Ty::F32 | Ty::F64)
    }
    pub fn is_scalar(self) -> bool {
        !matches!(self, Ty::Agg(_) | Ty::Unit)
    }
    /// Size in bytes for scalars; `None` for `Agg` (look up the layout) and `Unit` (0).
    pub fn scalar_size(self) -> Option<u32> {
        Some(match self {
            Ty::I8 | Ty::U8 | Ty::Bool => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I32 | Ty::U32 | Ty::F32 => 4,
            Ty::I64 | Ty::U64 | Ty::F64 | Ty::Ptr => 8,
            Ty::V128 => 16,
            Ty::Unit | Ty::Agg(_) => return None,
        })
    }
    /// Alignment in bytes of a scalar in memory: its size, except `V128` (8); `None` as for
    /// [`Ty::scalar_size`].
    pub fn scalar_align(self) -> Option<u32> {
        self.scalar_size().map(|n| n.min(8))
    }
}

/// Memory layout of an aggregate. For enums, lowering emits one "view" aggregate per variant
/// (tag + payload fields at their offsets) and uses `Proj::Cast` to reinterpret.
#[derive(Clone, Debug)]
pub struct AggLayout {
    /// Debug name, e.g. `string`, `Point`, `Shape::Circle`.
    pub name: String,
    pub size: u32,
    pub align: u32,
    /// (field type, byte offset). Field types may themselves be `Agg`.
    pub fields: Vec<(Ty, u32)>,
}

/// `AggId(0)` is always the rt string `VeltStr { w0: U64, len: U64, cap: U64 }` (size 24, align 8).
/// Word 0 holds the data pointer of the static / heap forms as a full `U64` (cast it to `Ptr` to
/// use it): with a `Ptr` field, bytes 4..8 would be padding on wasm32, yet an inline string keeps
/// text there, and aggregate copies need not preserve padding.
pub const STR_AGG: AggId = AggId(0);

/// `Program::default()` is the empty program, a starting point for builders and struct update
/// syntax (`Program { aggs, funcs, ..Default::default() }`) where a new field gets its "no
/// information" default. It is not a valid program: callers supply `aggs` with the string
/// layout first (`STR_AGG`) and `funcs` with the exported `velt_main` (invariant 5).
#[derive(Clone, Debug, Default)]
pub struct Program {
    pub aggs: Vec<AggLayout>,
    pub funcs: Vec<Function>,
    pub externs: Vec<ExternFn>,
    pub statics: Vec<StaticData>,
    /// Source path per `FileId` (index = `SrcLoc::file`); empty without source locations.
    pub files: Vec<String>,
    /// Debug types of source variables, by `DebugTyId` (invariant 10); empty without them.
    pub debug_types: Vec<DebugTy>,
}

/// A source position: 1-based line and column (in bytes) in `Program::files[file]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SrcLoc {
    pub file: u32,
    pub line: u32,
    pub col: u32,
}

impl Program {
    pub fn agg(&self, id: AggId) -> &AggLayout {
        &self.aggs[id.0 as usize]
    }
    pub fn debug_ty(&self, id: DebugTyId) -> &DebugTy {
        &self.debug_types[id.0 as usize]
    }
    pub fn func(&self, id: FuncId) -> &Function {
        &self.funcs[id.0 as usize]
    }
    pub fn ext(&self, id: ExternId) -> &ExternFn {
        &self.externs[id.0 as usize]
    }
    /// Size and alignment of any non-Unit type.
    pub fn size_align(&self, ty: Ty) -> (u32, u32) {
        match ty {
            Ty::Agg(id) => {
                let l = self.agg(id);
                (l.size, l.align)
            }
            Ty::Unit => (0, 1),
            s => (s.scalar_size().unwrap(), s.scalar_align().unwrap()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Linkage {
    /// Visible to the linker (e.g. `velt_main`, poll fns referenced by rt).
    Export,
    /// Local to the object file.
    #[default]
    Internal,
    /// Defined by another object of the link (invariant 11): declared, never emitted.
    Import,
}

#[derive(Clone, Debug)]
pub struct Function {
    /// Mangled, unique symbol name.
    pub symbol: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub locals: Vec<LocalDecl>,
    pub blocks: Vec<BasicBlock>,
    pub linkage: Linkage,
    /// Per block: the source location of each statement, then of the terminator (invariant 8).
    /// Empty = no location information.
    pub locs: Vec<Vec<Option<SrcLoc>>>,
    /// Per param: aliasing / access facts callers guarantee (invariant 9). Empty = none.
    pub param_attrs: Vec<ParamAttrs>,
    /// The poll function of an async function instance, `(state: ptr, cx: ptr) -> u32`
    /// (rt_abi_async.md §1): param 0 is its frame, which the caller hands over exclusively for
    /// the duration of the call. Set by lowering; `velt_opt` keeps it on clones.
    pub is_poll: bool,
    /// A gated function (`targetFeature`, rt_abi.md "SIMD"): it runs only where the CPU has
    /// this extension (sema checks every call), so backends compile it with the extension's
    /// whole feature set and its SHA operations as the bare instructions. `None` for every
    /// other function. Inlining keeps a gated body out of callers with another target.
    pub target: Option<crate::simd::Ext>,
    /// No call of it returns normally: its declared result is `never` (it panics, throws, exits
    /// or loops forever), so every call is rare. Optimizers keep its body out of line (inlining
    /// it only grows the hot code around the call) and backends may compile it as cold. Set by
    /// lowering; `velt_opt` keeps it on clones. False promises nothing.
    pub cold: bool,
    /// Whether optimizers and backends must inline it, must not, or decide for themselves: the
    /// std decorators `@inline` and `@noInline` (std/lang/intrinsics/decorators.velt). Set by
    /// lowering; `velt_opt` keeps it on clones.
    pub inline: Inline,
    /// A function C calls (`@export("c_name")`, lowering's c_export.rs): an unwind started below
    /// it (a Velt panic that a `catchPanic` further up would stop) must not pass it into C
    /// frames, so the process aborts there. LLVM makes its calls `invoke`s whose landing pad
    /// calls `abort` (a `cleanuppad` on Windows), as clang does for `noexcept`; Cranelift emits
    /// it without unwind information, so the unwinder finds no way past it, except on Windows,
    /// whose unwinder takes a function without unwind information for a leaf (there it keeps
    /// it: docs/internals/design/c-ffi.md "Exported functions"). Its small integer parameters
    /// carry no extension attribute: the C caller may leave their upper bits unset (AAPCS64
    /// outside Darwin). False for every other function.
    pub stops_unwind: bool,
}

/// A function's inlining, as std's decorators set it (`vir::Function::inline`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Inline {
    /// Neither decorator: the optimizer's and the backend's own heuristics decide.
    #[default]
    Default,
    /// `@inline`: every call is inlined wherever `velt_opt`'s inliner runs, whatever the
    /// callee's size (LLVM `alwaysinline`). Sema rejects it on a recursive function.
    Always,
    /// `@noInline` (#1884): every call stays a call (LLVM `noinline`, without `cold`).
    Never,
}

/// What every caller guarantees about one `Ptr` param for the duration of the call
/// (invariant 9). The default is "nothing known".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ParamAttrs {
    /// Memory accessed through this pointer is not accessed through any pointer not derived
    /// from it during the call (LLVM `noalias`, Rust `&mut`).
    pub noalias: bool,
    /// The callee never writes through this pointer (or pointers derived from it).
    pub readonly: bool,
    /// Nothing changes the pointee's bytes during the call, through this pointer or any other,
    /// except the words of `Atomic`s (changed through `velt_atomic_*` calls; inspecting a value
    /// also reads them as plain memory): a plain read of the pointee outside an `Atomic` gives
    /// the same value anywhere in the call, so it may be done once on entry. An `Atomic` is the
    /// aggregate lowering names `Atomic`. Implied by nothing else: a `readonly` pointee may still
    /// change through other pointers.
    pub frozen: bool,
    /// The pointer is never null.
    pub nonnull: bool,
    /// At least this many bytes behind the pointer are valid to read (0 = unknown).
    pub dereferenceable: u64,
}

impl ParamAttrs {
    /// Carries no information (the attributes of an unannotated param).
    pub fn is_empty(&self) -> bool {
        *self == ParamAttrs::default()
    }
}

impl Function {
    /// A function with one unnamed local per param and no blocks, source locations or param
    /// facts. It is not valid until the caller supplies the blocks, entry block first
    /// (invariants 4 and 6), and any further locals after the params' (or replaces `locals`
    /// with its own, still starting with one per param). Build the rest with struct update
    /// syntax (`Function { locals, blocks, ..Function::new(..) }`), so adding a field touches
    /// only this constructor, where it gets its "no information" default.
    pub fn new(symbol: String, params: Vec<Ty>, ret: Ty, linkage: Linkage) -> Self {
        Function {
            symbol,
            locals: params.iter().map(|&ty| LocalDecl::new(ty, None)).collect(),
            params,
            ret,
            blocks: Vec::new(),
            linkage,
            locs: Vec::new(),
            param_attrs: Vec::new(),
            is_poll: false,
            target: None,
            cold: false,
            inline: Inline::Default,
            stops_unwind: false,
        }
    }

    /// Attributes of param `i` (the empty set when the function has none).
    pub fn param_attr(&self, i: usize) -> ParamAttrs {
        self.param_attrs.get(i).copied().unwrap_or_default()
    }

    /// Location of statement `stmt` of block `block` (`stmt == stmts.len()`: the terminator).
    /// Tolerates missing information.
    pub fn loc(&self, block: usize, stmt: usize) -> Option<SrcLoc> {
        self.locs.get(block)?.get(stmt).copied().flatten()
    }

    /// Location of the terminator of `block`.
    pub fn term_loc(&self, block: usize) -> Option<SrcLoc> {
        let n = self.blocks.get(block)?.stmts.len();
        self.loc(block, n)
    }

    /// First known location in the function (its declaration line, roughly).
    pub fn first_loc(&self) -> Option<SrcLoc> {
        self.locs.iter().flatten().find_map(|l| *l)
    }
}

#[derive(Clone, Debug)]
pub struct LocalDecl {
    pub ty: Ty,
    /// Source name for debugging, if any.
    pub name: Option<String>,
    /// The source variable this local holds, for debuggers (invariant 10).
    pub debug: Option<LocalDebug>,
}

impl LocalDecl {
    /// A local of type `ty`, named `name` in the source (`None` for temporaries).
    pub fn new(ty: Ty, name: Option<String>) -> Self {
        LocalDecl {
            ty,
            name,
            debug: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExternFn {
    pub symbol: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// Never returns (e.g. `velt_rt_panic`); calls to it are followed by `Unreachable`.
    pub noreturn: bool,
    /// A host function (additive): not a link symbol but the WebAssembly import this names
    /// (`symbol` is only its name in the object). `None` for every other extern.
    pub host: Option<HostImport>,
    /// A C function (`declare c`, additive): where it is declared (`path:line:col`), for the
    /// dev JIT's error when the symbol is in no loaded library. `None` for every other extern.
    pub c_decl: Option<String>,
    /// A variadic C function (`...` in C; additive): `params` are its fixed parameters, and a
    /// call passes at least as many arguments, the further ones scalars of type `I32`, `U32`,
    /// `I64`, `U64`, `F64` or `Ptr` (C's default argument promotions already applied). Backends
    /// call it with the target's variadic convention. `false` for every other extern.
    pub variadic: bool,
}

/// Read-only data (string literal bytes, vtables, …), or a word the program writes.
#[derive(Clone, Debug, Default)]
pub struct StaticData {
    pub bytes: Vec<u8>,
    pub align: u32,
    /// Absolute 8-byte addresses patched into `bytes` at link time (e.g. vtable slots). Each
    /// offset is 8-aligned and the 8 bytes there are zero in `bytes`. `target` is a
    /// `Const::Func`, `Const::Extern` or `Const::Static`.
    pub relocs: Vec<(u32, Const)>,
    /// `Some(id)`: writable data, the word of a static atomic (a module `const` made by
    /// `new Atomic<T>(literal)`), with no relocations, which code reads and writes only
    /// through `velt_atomic_*` externs. `bytes` is its starting value, and no pass may assume
    /// it still holds it. `id` names it across the versions of a `velt dev` session, which
    /// all use the session's one copy: the 64-bit FNV-1a hash of the constant's qualified
    /// name and value type (`tls::stable_id`), so an edit changing the type gets a new word.
    /// It is one aligned word (1, 2, 4 or 8 bytes). Every other static is read-only (`None`).
    pub mutable: Option<u64>,
    /// The static's symbol (invariant 13): `None` unless `linkage` is `Export` or `Import`.
    pub symbol: Option<String>,
    /// `Internal` (the default): private to the program's objects. `Export`: defined here,
    /// visible to the link's other objects under `symbol`. `Import`: defined by another object
    /// under `symbol`, with the contents above for reading only (invariant 13).
    pub linkage: Linkage,
    /// A C library's variable (`declare c("lib") let`, additive): an `Import` under `symbol`,
    /// no `bytes`, `relocs` or `mutable`; backends declare it as C does an `extern` variable
    /// (default visibility, writable, contents unknown). `false` for every other static.
    pub c_global: bool,
}

#[derive(Clone, Debug)]
pub struct BasicBlock {
    pub stmts: Vec<Stmt>,
    pub term: Terminator,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Proj {
    /// Field `n` of the current `Agg` type.
    Field(u32),
    /// Current type is `Ptr`; continue at the pointee, which has type `Ty`.
    Deref(Ty),
    /// Reinterpret the current memory as another aggregate (enum variant views).
    Cast(AggId),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub local: Local,
    pub proj: Vec<Proj>,
}

impl Place {
    pub fn local(l: Local) -> Self {
        Place {
            local: l,
            proj: vec![],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    /// Integer of any int type, or `Ptr` (e.g. null = 0), or Bool as 0/1.
    Int(i128),
    Float(f64),
    Bool(bool),
    Unit,
    /// Address of a static (type `Ptr`).
    Static(StaticId),
    /// Address of a function (type `Ptr`).
    Func(FuncId),
    /// Address of an extern function (type `Ptr`).
    Extern(ExternId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    /// Read (bitwise copy) of a place. For `Agg` places this is a memcpy.
    Copy(Place),
    Const(Const, Ty),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    /// Integer/float negation (wrapping for ints).
    Neg,
    /// Bool not.
    Not,
    /// Bitwise not on ints.
    BitNot,
}

/// Operands have identical types, except `PtrAdd` (Ptr, I64/U64) and shifts (rhs any int).
/// Int arithmetic wraps; Div/Rem by zero are guarded by lowering (it emits a check + panic).
/// Signedness comes from the operand `Ty`. Comparisons produce `Bool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Pointer + byte offset → Ptr.
    PtrAdd,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Rvalue {
    Use(Operand),
    Unary(UnOp, Operand),
    Binary(BinOp, Operand, Operand),
    /// Numeric conversion to the destination type with Rust `as` semantics
    /// (int↔int truncate/extend by source signedness, float→int saturating, int→float, Bool→int, int↔Ptr).
    Cast(Operand, Ty),
    /// Address of a place → `Ptr`. The place's local must live in memory (backend decides).
    AddrOf(Place),
    /// Build an aggregate from field operands in field order.
    Aggregate(AggId, Vec<Operand>),
    /// A pure operation the compiler knows (`Intrinsic`): reads only its operands, touches no
    /// memory, never panics or unwinds, so passes may fold, duplicate, move or delete it like
    /// arithmetic. Its operand and result types are the operation's signature.
    Intrinsic(Intrinsic, Vec<Operand>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stmt {
    Assign(Place, Rvalue),
    /// Copy `size` bytes from `src` pointer to `dst` pointer (non-overlapping).
    MemCopy {
        dst: Operand,
        src: Operand,
        size: u64,
    },
    /// Copy `len` (U64 operand) bytes; regions may overlap when `overlapping` (memmove).
    MemCopyDyn {
        dst: Operand,
        src: Operand,
        len: Operand,
        overlapping: bool,
    },
    /// Fill `len` (U64 operand) bytes at `dst` with the low byte of `byte` (U8 operand).
    MemSet {
        dst: Operand,
        byte: Operand,
        len: Operand,
    },
    Nop,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Callee {
    Func(FuncId),
    Extern(ExternId),
    /// Indirect call through a `Ptr` operand; signature given explicitly.
    Ptr {
        target: Operand,
        params: Vec<Ty>,
        ret: Ty,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Terminator {
    Goto(BlockId),
    Branch {
        cond: Operand,
        then: BlockId,
        els: BlockId,
    },
    /// Jump on an integer value.
    Switch {
        value: Operand,
        cases: Vec<(i128, BlockId)>,
        default: BlockId,
    },
    /// `Const::Unit` for functions returning `Unit`.
    Return(Operand),
    /// `dest` is `None` for Unit-returning callees. Noreturn callees still name a `next` block
    /// (which lowering fills with `Unreachable`).
    Call {
        callee: Callee,
        args: Vec<Operand>,
        dest: Option<Place>,
        next: BlockId,
        /// Where a Velt exception unwinding out of the callee continues (invariant 14): a
        /// cleanup pad, with `dest` not written. `None` in every program that doesn't catch.
        unwind: Option<BlockId>,
    },
    Unreachable,
}

// ───────────────────────────── Display (debug dumps) ─────────────────────────────
// Formatting may change freely but must stay deterministic (used in snapshot tests).

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Ty::I8 => "i8",
            Ty::I16 => "i16",
            Ty::I32 => "i32",
            Ty::I64 => "i64",
            Ty::U8 => "u8",
            Ty::U16 => "u16",
            Ty::U32 => "u32",
            Ty::U64 => "u64",
            Ty::F32 => "f32",
            Ty::F64 => "f64",
            Ty::Bool => "bool",
            Ty::Ptr => "ptr",
            Ty::V128 => "v128",
            Ty::Unit => "unit",
            Ty::Agg(a) => return write!(f, "agg#{}", a.0),
        };
        f.write_str(s)
    }
}

fn fmt_tys(tys: &[Ty]) -> String {
    tys.iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

impl fmt::Display for ParamAttrs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = vec![];
        if self.noalias {
            parts.push("noalias".to_string());
        }
        if self.readonly {
            parts.push("readonly".to_string());
        }
        if self.frozen {
            parts.push("frozen".to_string());
        }
        if self.nonnull {
            parts.push("nonnull".to_string());
        }
        if self.dereferenceable > 0 {
            parts.push(format!("dereferenceable({})", self.dereferenceable));
        }
        write!(f, "[{}]", parts.join(" "))
    }
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = format!("_{}", self.local.0);
        for p in &self.proj {
            s = match p {
                Proj::Field(n) => format!("{s}.{n}"),
                Proj::Deref(t) => format!("(*{s} as {t})"),
                Proj::Cast(a) => format!("({s} as agg#{})", a.0),
            };
        }
        f.write_str(&s)
    }
}

impl fmt::Display for Const {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Const::Int(v) => write!(f, "{v}"),
            Const::Float(v) => write!(f, "{v:?}"),
            Const::Bool(b) => write!(f, "{b}"),
            Const::Unit => f.write_str("()"),
            Const::Static(s) => write!(f, "static#{}", s.0),
            Const::Func(x) => write!(f, "fn#{}", x.0),
            Const::Extern(x) => write!(f, "extern#{}", x.0),
        }
    }
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operand::Copy(p) => write!(f, "{p}"),
            Operand::Const(c @ (Const::Int(_) | Const::Float(_)), t) => write!(f, "{c}_{t}"),
            Operand::Const(c, _) => write!(f, "{c}"),
        }
    }
}

impl fmt::Display for Rvalue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rvalue::Use(o) => write!(f, "{o}"),
            Rvalue::Unary(op, o) => write!(f, "{} {o}", format!("{op:?}").to_lowercase()),
            Rvalue::Binary(op, a, b) => write!(f, "{} {a}, {b}", format!("{op:?}").to_lowercase()),
            Rvalue::Cast(o, t) => write!(f, "cast {o} as {t}"),
            Rvalue::AddrOf(p) => write!(f, "&{p}"),
            Rvalue::Aggregate(a, ops) => {
                let ops: Vec<String> = ops.iter().map(|o| o.to_string()).collect();
                write!(f, "agg#{} {{ {} }}", a.0, ops.join(", "))
            }
            Rvalue::Intrinsic(i, ops) => {
                let ops: Vec<String> = ops.iter().map(|o| o.to_string()).collect();
                write!(f, "{}({})", i.name(), ops.join(", "))
            }
        }
    }
}

impl fmt::Display for Stmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stmt::Assign(p, rv) => write!(f, "{p} = {rv}"),
            Stmt::MemCopy { dst, src, size } => write!(f, "memcopy {dst}, {src}, {size}"),
            Stmt::MemCopyDyn {
                dst,
                src,
                len,
                overlapping,
            } => {
                let op = if *overlapping { "memmove" } else { "memcopy" };
                write!(f, "{op} {dst}, {src}, {len}")
            }
            Stmt::MemSet { dst, byte, len } => write!(f, "memset {dst}, {byte}, {len}"),
            Stmt::Nop => f.write_str("nop"),
        }
    }
}
