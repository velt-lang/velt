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
//! 4. Block 0 is the entry block. Locals `0..params.len()` hold the incoming params.
//! 5. Lowering emits the program entry as an exported function `velt_main() -> I32`
//!    (see rt_abi.md); backends need no special casing.
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
//!    set on `Ptr` params and only where sema's exclusivity guarantees hold (docs/reference/memory.md
//!    \"Exclusive access"): during a call, memory reachable through a `BorrowMut` param is
//!    reachable through no other param, and `Borrow` params never alias a `BorrowMut`/`Owned`
//!    one. Lowering sets them for user functions from `PassMode` (`BorrowMut` aggregate or
//!    class object → `noalias`, only when values of the type have no other owners: not counted
//!    and never borrowed inside a counted object, docs/design/semantics-stage2.md §3.4;
//!    `Borrow` aggregate or class object → `readonly` unless the value holds a `Mutex` or the
//!    param is written (`LocalDef::mutable`); aggregate-return out-pointer → `noalias`;
//!    `nonnull` + `dereferenceable(size)` on all of them; the frame of a poll function
//!    (`Function::is_poll`) → `nonnull` + `dereferenceable(frame size)`, plus `noalias` from
//!    `velt_opt` when no pointer into the frame ever leaves the function). Every caller must
//!    uphold them: `velt_opt` keeps them on clones and specializations and drops a param's
//!    attributes when it rewrites that param. Backends may use them (LLVM param attributes) or
//!    ignore them.

use std::fmt;

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
            Ty::Unit | Ty::Agg(_) => return None,
        })
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

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub aggs: Vec<AggLayout>,
    pub funcs: Vec<Function>,
    pub externs: Vec<ExternFn>,
    pub statics: Vec<StaticData>,
    /// Source path per `FileId` (index = `SrcLoc::file`); empty without source locations.
    pub files: Vec<String>,
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
            s => {
                let n = s.scalar_size().unwrap();
                (n, n)
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Linkage {
    /// Visible to the linker (e.g. `velt_main`, poll fns referenced by rt).
    Export,
    /// Local to the object file.
    Internal,
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
    /// A function with no locals, blocks, source locations or param facts. Build the rest with
    /// struct update syntax (`Function { locals, blocks, ..Function::new(..) }`), so adding a
    /// field touches only this constructor.
    pub fn new(symbol: String, params: Vec<Ty>, ret: Ty, linkage: Linkage) -> Self {
        Function {
            symbol,
            params,
            ret,
            locals: Vec::new(),
            blocks: Vec::new(),
            linkage,
            locs: Vec::new(),
            param_attrs: Vec::new(),
            is_poll: false,
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
}

impl LocalDecl {
    /// A local of type `ty`, named `name` in the source (`None` for temporaries).
    pub fn new(ty: Ty, name: Option<String>) -> Self {
        LocalDecl { ty, name }
    }
}

#[derive(Clone, Debug)]
pub struct ExternFn {
    pub symbol: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// Never returns (e.g. `velt_rt_panic`); calls to it are followed by `Unreachable`.
    pub noreturn: bool,
}

/// Read-only data (string literal bytes, vtables, …).
#[derive(Clone, Debug)]
pub struct StaticData {
    pub bytes: Vec<u8>,
    pub align: u32,
    /// Absolute 8-byte addresses patched into `bytes` at link time (e.g. vtable slots). Each
    /// offset is 8-aligned and the 8 bytes there are zero in `bytes`. `target` is a
    /// `Const::Func`, `Const::Extern` or `Const::Static`.
    pub relocs: Vec<(u32, Const)>,
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

impl Program {
    fn fmt_callee(&self, c: &Callee) -> String {
        match c {
            Callee::Func(id) => match self.funcs.get(id.0 as usize) {
                Some(func) => format!("fn#{} {}", id.0, func.symbol),
                None => format!("fn#{}", id.0),
            },
            Callee::Extern(id) => match self.externs.get(id.0 as usize) {
                Some(e) => format!("extern#{} {}", id.0, e.symbol),
                None => format!("extern#{}", id.0),
            },
            Callee::Ptr {
                target,
                params,
                ret,
            } => format!("({target}: fn({}) -> {ret})", fmt_tys(params)),
        }
    }

    fn fmt_term(&self, t: &Terminator) -> String {
        match t {
            Terminator::Goto(b) => format!("goto bb{}", b.0),
            Terminator::Branch { cond, then, els } => {
                format!("branch {cond}, bb{}, bb{}", then.0, els.0)
            }
            Terminator::Switch {
                value,
                cases,
                default,
            } => {
                let cs: Vec<String> = cases
                    .iter()
                    .map(|(v, b)| format!("{v} => bb{}", b.0))
                    .collect();
                format!(
                    "switch {value} [{}], default bb{}",
                    cs.join(", "),
                    default.0
                )
            }
            Terminator::Return(o) => format!("return {o}"),
            Terminator::Call {
                callee,
                args,
                dest,
                next,
            } => {
                let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                let call = format!("call {}({})", self.fmt_callee(callee), args.join(", "));
                match dest {
                    Some(d) => format!("{d} = {call} -> bb{}", next.0),
                    None => format!("{call} -> bb{}", next.0),
                }
            }
            Terminator::Unreachable => "unreachable".into(),
        }
    }
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, a) in self.aggs.iter().enumerate() {
            let fields: Vec<String> = a.fields.iter().map(|(t, o)| format!("{t}@{o}")).collect();
            writeln!(
                f,
                "agg#{} {} size={} align={} {{ {} }}",
                i,
                a.name,
                a.size,
                a.align,
                fields.join(", ")
            )?;
        }
        for (i, s) in self.statics.iter().enumerate() {
            writeln!(
                f,
                "static#{} align={} {:?}",
                i,
                s.align,
                String::from_utf8_lossy(&s.bytes)
            )?;
        }
        for (i, e) in self.externs.iter().enumerate() {
            let nr = if e.noreturn { " noreturn" } else { "" };
            writeln!(
                f,
                "extern#{} {}({}) -> {}{}",
                i,
                e.symbol,
                fmt_tys(&e.params),
                e.ret,
                nr
            )?;
        }
        for (i, func) in self.funcs.iter().enumerate() {
            let link = match func.linkage {
                Linkage::Export => "export",
                Linkage::Internal => "internal",
            };
            writeln!(f)?;
            writeln!(
                f,
                "fn#{} {} {}({}) -> {} {{",
                i,
                link,
                func.symbol,
                fmt_tys(&func.params),
                func.ret
            )?;
            for (li, l) in func.locals.iter().enumerate() {
                let kind = if li < func.params.len() {
                    "param"
                } else {
                    "let"
                };
                let attrs = func.param_attr(li);
                let attrs = if li < func.params.len() && !attrs.is_empty() {
                    format!(" {attrs}")
                } else {
                    String::new()
                };
                match &l.name {
                    Some(n) => writeln!(f, "  {kind} _{li}: {}{attrs} // {n}", l.ty)?,
                    None => writeln!(f, "  {kind} _{li}: {}{attrs}", l.ty)?,
                }
            }
            for (bi, b) in func.blocks.iter().enumerate() {
                writeln!(f, "  bb{bi}:")?;
                for s in &b.stmts {
                    writeln!(f, "    {s}")?;
                }
                writeln!(f, "    {}", self.fmt_term(&b.term))?;
            }
            writeln!(f, "}}")?;
        }
        Ok(())
    }
}
