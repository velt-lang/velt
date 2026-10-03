//! HIR: fully name-resolved, type-checked, ownership-annotated program.
//! Produced by `velt_sema::check`, consumed by `velt_vir::lower`.
//! CONTRACT FILE — maintainer-owned (see docs/internals/contracts/README.md).
//!
//! Sections marked "M1" are frozen. Sections marked "M2+ draft" exist so downstream code can be
//! written against them, but they may still change at a milestone boundary.
//!
//! Desugarings sema performs (so lowering never sees them):
//! - Template literals → `Intrinsic::StrConcat` / `Intrinsic::ToString` calls.
//! - C-style `for` → `Block { init; While { cond, body, step } }`.
//! - `do { } while (c)` → `While { cond: true, body: { body; if (!c) break; } }`, or, when the body
//!   contains a `continue` targeting this loop, `While { cond: true, body, step: Some({ if (!c) break; }) }`
//!   — so lowering must support `break` inside `step` (it targets the same loop).
//! - `++x` as a value → `Block { [CompoundAssign], value: Local(x) }`; `x++` as a value →
//!   `Block { [Let tmp = x; CompoundAssign], value: Local(tmp) }`.
//! - Assignment places are `Local(id, UseMode::BorrowMut)` (a write, not a read).
//! - `StrConcat`/`ToString` operands may be owned temporaries; lowering drops them after the call.
//! - Values are shared (semantics stage 2): a place of a shared value (`Ctx::is_shared_value`:
//!   strings, objects, arrays, maps, closures) is never left moved-from where it is used again or
//!   cannot be moved from (borrowed params, fields of classes, array / `for...of` elements); such
//!   uses are `Intrinsic::Share` calls (a count increment at most, hir_encodings.md "Sharing")
//!   or `Capture::share` captures. Moves remain where the source is dead (no count traffic).
//! - Compound assignment on locals/fields stays as `CompoundAssign` (place evaluated once).
//! - `x++`/`x--` → `CompoundAssign` (+ value read for postfix via a temp `Let`).
//! - Ternary `c ? a : b` → `ExprKind::If`.
//! - Implicit `return` of `Unit` at end of `void` functions is NOT inserted; lowering handles it.

//!
//! M2/M3 encodings (classes, vtables, interfaces, closures, errors, async, …) are specified in
//! `docs/internals/contracts/hir_encodings.md` — part of this contract.

mod intrinsic;

use velt_common::Span;

pub use intrinsic::Intrinsic;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DefId(pub u32);

/// Index into `Body::locals` of the enclosing function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

/// Index into `TyTable`. Types are interned: equal types ⇔ equal ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TyId(pub u32);

// ───────────────────────────── Types (M1) ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    ISize,
    U8,
    U16,
    U32,
    U64,
    USize,
}

impl IntTy {
    pub fn is_signed(self) -> bool {
        matches!(
            self,
            IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::ISize
        )
    }
    /// Width in bits, assuming a 64-bit target for isize/usize.
    pub fn bits(self) -> u32 {
        match self {
            IntTy::I8 | IntTy::U8 => 8,
            IntTy::I16 | IntTy::U16 => 16,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::I64 | IntTy::U64 | IntTy::ISize | IntTy::USize => 64,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FloatTy {
    F32,
    F64,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TyKind {
    // M1
    Int(IntTy),
    Float(FloatTy),
    Bool,
    /// Owned UTF-8 string, rt layout `VeltStr { ptr, len, cap }` (see rt_abi.md).
    Str,
    /// `void`
    Unit,
    /// Type of `return`/`break`/`throw`/panicking expressions.
    Never,
    /// Placeholder after a reported type error; lowering never sees it (check fails first).
    Error,

    // M2+ draft
    /// Instance of a struct/class/enum (`DefId` points at `Def::Adt`/`Def::Enum`) with type args.
    Adt(DefId, Vec<TyId>),
    Array(TyId),
    Map(TyId, TyId),
    Tuple(Vec<TyId>),
    /// `T | null`
    Option(TyId),
    /// Lowering-internal `Result<ret, E>` ABI of throwing functions (sema never produces it).
    Result(TyId, TyId),
    /// `Promise<T, E>`: resolves to `T` or rejects with `E` (`Never`: never rejects).
    Promise(TyId, TyId),
    /// `shared<T>` — atomically reference counted.
    Shared(TyId),
    /// Function value (closure or named function, hir_encodings.md); may throw `throws`.
    FnPtr {
        params: Vec<TyId>,
        ret: TyId,
        throws: TyId,
    },
    /// Closure value: unique type per closure expression (`DefId` of its `Def::Fn`).
    Closure(DefId),
    /// Generic parameter #n of the enclosing generic definition.
    Param(u32),
    /// Interface value (`Named` used as a type): fat pointer (data, vtable). M2.
    Dyn(DefId, Vec<TyId>),
    /// Literal type (`"circle"`, `42`, `true`; see hir_encodings.md "Literal types"): its only
    /// value is `LitValue`. Zero-sized: a value carries no bits (like `Unit`).
    Literal(LitValue),
}

/// The value of a literal type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LitValue {
    Str(String),
    Int(IntTy, i128),
    /// `f64::to_bits` of the value (converted to `FloatTy` when printed).
    Float(FloatTy, u64),
    Bool(bool),
}

#[derive(Clone, Debug, Default)]
pub struct TyTable {
    kinds: Vec<TyKind>,
    map: std::collections::HashMap<TyKind, TyId>,
}

impl TyTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, kind: TyKind) -> TyId {
        if let Some(&id) = self.map.get(&kind) {
            return id;
        }
        let id = TyId(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.map.insert(kind, id);
        id
    }

    pub fn kind(&self, id: TyId) -> &TyKind {
        &self.kinds[id.0 as usize]
    }

    /// Lookup without inserting.
    pub fn get(&self, kind: &TyKind) -> Option<TyId> {
        self.map.get(kind).copied()
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }
}

// ─────────────────────────── Program & defs ───────────────────────────

#[derive(Clone, Debug)]
pub struct Program {
    pub types: TyTable,
    /// Indexed by `DefId`.
    pub defs: Vec<Def>,
    /// The user's `main` function (M1: `function main(): void | i32`; M3: may be async).
    /// `None` only for a library root checked with `CheckOptions { require_main: false }`
    /// (`velt check`); lowering requires `Some`.
    pub entry: Option<DefId>,
    /// Interface implementations (M2): which concrete type implements which interface, with
    /// the method defs in `InterfaceDef::methods` order (defaults already substituted).
    pub impls: Vec<ImplDef>,
}

/// `ty` implements `iface<iface_args>` using `methods` (one per interface method, in order).
/// Generic impls use `TyKind::Param` in `ty`/`iface_args`; lowering instantiates them.
#[derive(Clone, Debug)]
pub struct ImplDef {
    pub ty: TyId,
    pub generics: u32,
    pub iface: DefId,
    pub iface_args: Vec<TyId>,
    pub methods: Vec<DefId>,
}

impl Program {
    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.0 as usize]
    }
}

#[derive(Clone, Debug)]
pub enum Def {
    Fn(FnDef),
    /// `declare function` — external C-ABI symbol provided by velt_rt or a native lib.
    ExternFn(ExternFnDef),
    // M2+ draft
    Adt(AdtDef),
    Enum(EnumDef),
    Global(GlobalDef),
    Interface(InterfaceDef),
}

/// How an argument is passed / a parameter is received. Decided by sema, part of the fn ABI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassMode {
    /// Bitwise copy (Copy types: ints, floats, bool, Copy structs).
    Copy,
    /// Callee receives a read-only pointer; caller keeps ownership. Default for non-Copy params.
    Borrow,
    /// Callee receives a mutable pointer: sema inferred that it may modify the value (a param
    /// whose contents the body mutates, a method that mutates `this`; see hir_encodings.md).
    BorrowMut,
    /// Ownership moves into the callee (callee drops it).
    Owned,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub local: LocalId,
    pub ty: TyId,
    pub mode: PassMode,
}

#[derive(Clone, Debug)]
pub struct FnDef {
    /// Fully qualified, unique, human-readable name: `main`, `User.greet`, `std/fs::readFile`,
    /// `main::{closure#0}`. Lowering mangles it into a symbol.
    pub name: String,
    /// Number of generic type params (`TyKind::Param(0..n)`).
    pub generics: u32,
    pub params: Vec<Param>,
    pub ret: TyId,
    pub is_async: bool,
    /// For methods: the `this` type (first param is `this`).
    pub self_ty: Option<TyId>,
    /// For closures: captured variables become the leading params, in this order.
    pub captures: Vec<Capture>,
    pub body: Body,
    /// Thrown error type (a union when several types can be thrown; see hir_encodings.md
    /// "Errors"). May mention type params; `Some(Never)` after substitution means non-throwing.
    pub throws: Option<TyId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ExternFnDef {
    pub name: String,
    /// Link symbol name (same as `name` without module path).
    pub symbol: String,
    pub params: Vec<TyId>,
    pub ret: TyId,
    /// M3: `declare async function f(...): Promise<T>` — the symbol returns an rt leaf future
    /// (`VeltFut*`, result slot at offset 16; see rt_abi_async.md); `ret` is `Promise<T>`.
    pub is_async: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct LocalDef {
    pub name: String,
    pub ty: TyId,
    /// Assigned (`let` locals, reassigned params) or, for params, contents modified by the body
    /// (see hir_encodings.md "Mutation inference").
    pub mutable: bool,
    /// The variable lives in a counted cell shared with the escaping closures that capture it
    /// (semantics stage 2, hir_encodings.md "Sharing"): one of them or the enclosing function
    /// assigns it while another still sees it. Set on the enclosing local and on each capturing
    /// closure's capture local; the variable is never moved from.
    pub boxed: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Body {
    /// Params are the first `params.len()` locals.
    pub locals: Vec<LocalDef>,
    pub block: Block,
}

// M2+ draft ------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdtKind {
    /// Value type, stored inline.
    Struct,
    /// Heap allocated, uniquely owned, moved.
    Class,
    /// Anonymous object literal type synthesized by sema.
    Anon,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub name: String,
    pub ty: TyId,
    /// Default initializer (class field `= expr`, or `null` for optional fields).
    pub default: Option<Expr>,
    /// Declared `private` (in this type or the base class that declares it). Interface fields
    /// are never private.
    pub private: bool,
}

#[derive(Clone, Debug)]
pub struct AdtDef {
    pub name: String,
    pub kind: AdtKind,
    pub generics: u32,
    pub fields: Vec<FieldDef>,
    /// Sema's verdict: bitwise-copyable (all fields Copy, kind Struct/Anon).
    pub is_copy: bool,
    /// Some field (own or inherited) is `private`. Such a type has no JSON form: decoding could
    /// forge the runtime handles std types keep in private fields, and writing would leak them.
    pub private_fields: bool,
    /// Some field is assigned somewhere in the program (`x.f = …`, `x.f += …`): two references
    /// to one value must see the same fields, so sharing it needs one counted object
    /// (hir_encodings.md "Sharing"); otherwise a share may copy it field by field.
    pub assigned: bool,
    /// Classes: base class type (its fields are a prefix of `fields`).
    pub base: Option<TyId>,
    /// Classes: constructor `Def::Fn` (None: every field has a default).
    pub ctor: Option<DefId>,
    /// Drop hook: the type's `[Symbol.dispose]()` method (modifies `this`, no params, void). Drop glue calls it
    /// first, then drops the fields. Types with a dispose hook are never Copy.
    pub dispose: Option<DefId>,
    /// Classes: virtual method slots (only methods overridden somewhere), base slots first.
    /// For a subclass, the slot holds its own override or the inherited method.
    pub vtable: Vec<DefId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct InterfaceDef {
    pub name: String,
    pub generics: u32,
    pub fields: Vec<FieldDef>,
    /// Methods in slot order.
    pub methods: Vec<InterfaceMethodDef>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct InterfaceMethodDef {
    pub name: String,
    /// `Def::Fn` of the default body; its `self_ty` is `TyKind::Param(generics)` (an implicit
    /// extra type param standing for the implementor).
    pub default: Option<DefId>,
    /// The method returns a promise that carries its errors (sema's dispatch group of the slot
    /// holds such a method): its implementations reject the promise, so a call through the slot
    /// never throws (hir_encodings.md "Errors").
    pub promise: bool,
    /// What a call through the slot throws, in terms of the interface's type params (`E` of
    /// `next(): IteratorResult<T> throws E`): substituted with the `Dyn`'s type args. `None`
    /// for a promise slot or one that cannot throw.
    pub throws: Option<TyId>,
}

#[derive(Clone, Debug)]
pub struct VariantDef {
    pub name: String,
    pub payload: Vec<TyId>,
    pub discriminant: i64,
    /// String enum member (`Up = "UP"`): its string value (printed, `JSON.stringify`d and
    /// converted to `string` instead of the discriminant).
    pub str_value: Option<String>,
}

#[derive(Clone, Debug)]
pub struct EnumDef {
    pub name: String,
    pub generics: u32,
    pub variants: Vec<VariantDef>,
    pub is_copy: bool,
    /// Compiler-generated union type `A | B | ...` (see hir_encodings.md "Union types"): one
    /// variant per member with that member as its single payload; printing, `ToString` and
    /// `JSON.stringify` show the active member's value.
    pub is_union: bool,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct GlobalDef {
    pub name: String,
    pub ty: TyId,
    pub init: Expr,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct Capture {
    /// Local in the *enclosing* function.
    pub outer: LocalId,
    /// Corresponding param local in the closure body.
    pub inner: LocalId,
    pub mode: PassMode,
    /// An `Owned` capture that shares instead of moving (`Intrinsic::Share` semantics): the
    /// enclosing variable is used after the closure is created, or cannot be moved from (a
    /// borrowed param, a `for...of` element). The enclosing local stays initialized.
    pub share: bool,
}

// ───────────────────────────── Statements (M1) ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Trailing value expression (used for block-valued `match` arms etc.). Usually `None`.
    pub value: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    /// `let`/`const` with a simple binding. `init` None ⇒ uninitialized until assigned.
    Let {
        local: LocalId,
        init: Option<Expr>,
    },
    /// M2+ draft: destructuring `let { a, b } = e` / `let [x, y] = e`.
    LetPat {
        pat: Pat,
        init: Expr,
    },
    Expr(Expr),
    Return(Option<Expr>),
    If {
        cond: Expr,
        then: Block,
        els: Option<Block>,
    },
    /// `step` runs after the body and on `continue` (from desugared C-style `for`).
    While {
        label: Option<String>,
        cond: Expr,
        body: Block,
        step: Option<Expr>,
    },
    /// `for (const x of iter)` over arrays (elements borrowed, or copied if Copy).
    ForOf {
        label: Option<String>,
        binding: Pat,
        iter: Expr,
        body: Block,
        /// The loop consumes `iter` (an owned temporary array, see hir_encodings.md): each
        /// element is moved into `binding` (owned bindings, `UseMode::Move`); elements not yet
        /// visited when the loop is left early are dropped, then the buffer is freed.
        consume: bool,
    },
    /// `try { body } catch (e) { handler } finally { fin }`: the catch local has the union `body`
    /// can throw (`None` / `Never`: nothing is caught; see hir_encodings.md "Errors").
    Try {
        body: Block,
        catch: Option<(Option<LocalId>, Block)>,
        finally: Option<Block>,
    },
    Break(Option<String>),
    Continue(Option<String>),
    Block(Block),
}

// ───────────────────────────── Expressions ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub ty: TyId,
    pub span: Span,
}

/// How a place is used by the expression that reads it (sema's ownership analysis).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UseMode {
    Copy,
    /// Ownership leaves the place; lowering must not drop it at scope end (drop flags if conditional).
    Move,
    Borrow,
    BorrowMut,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Lit {
    /// Value already range-checked against `Expr::ty`; negative numbers are `Unary(Neg, ..)`.
    Int(u128),
    Float(f64),
    Str(String),
    Bool(bool),
    Unit,
    /// `null` (type is `Option<T>`), M2+.
    Null,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

/// Operand types are identical (sema inserts no implicit conversions).
/// Comparison ops yield `Bool`. On `Str`, only `Eq`/`NotEq`/`Lt..GtEq` are valid (string concat
/// is `Intrinsic::StrConcat`). Int `Div`/`Rem` trap on zero; `Shr` is arithmetic for signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    UShr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogicOp {
    And,
    Or,
}

#[derive(Clone, Debug)]
pub enum Callee {
    /// Direct call to a `Def::Fn` or `Def::ExternFn`, with generic args (empty in M1).
    Def(DefId, Vec<TyId>),
    /// Call through a function value (closure), M2.
    Indirect(Box<Expr>),
    /// Virtual method call: receiver is `args[0]` (a class instance, borrowed); `slot` indexes
    /// `AdtDef::vtable` of the receiver's static class. M2.
    Virtual {
        slot: u32,
    },
    /// Interface method call on a `Dyn` receiver `args[0]`; `slot` indexes `InterfaceDef::methods`.
    Dyn {
        slot: u32,
    },
    /// Method call on a receiver of type `TyKind::Param(n)` bounded by `iface` (`<T extends I>`);
    /// resolved statically via `Program::impls` after monomorphization. M2.
    ParamMethod {
        iface: DefId,
        iface_args: Vec<TyId>,
        slot: u32,
        /// Type arguments of a generic interface method's own type params; they follow the
        /// implementing method's owner type args (empty for non-generic methods).
        method_type_args: Vec<TyId>,
    },
    Intrinsic(Intrinsic),
}

#[derive(Clone, Debug)]
pub struct Arm {
    pub pat: Pat,
    pub guard: Option<Expr>,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    // M1
    Lit(Lit),
    Local(LocalId, UseMode),
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Short-circuit `&&` / `||` on Bool.
    Logical {
        op: LogicOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `place = value`. `place` is a place expression (Local/Field/Index). Old value is dropped.
    Assign {
        place: Box<Expr>,
        value: Box<Expr>,
    },
    /// `place op= value`, type Unit.
    CompoundAssign {
        op: BinOp,
        place: Box<Expr>,
        value: Box<Expr>,
    },
    /// Args already reordered/defaulted to match params; each arg's use mode matches the param's PassMode.
    Call {
        callee: Callee,
        args: Vec<Expr>,
    },
    /// Numeric casts `x as f64`, `x as u8` (truncating/saturating like Rust `as`).
    Cast(Box<Expr>),
    /// Value-producing if (from ternary). Both branches have `Expr::ty`.
    If {
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    Block(Block),

    // M2+ draft
    Global(DefId),
    /// Function item used as a value (fn pointer).
    FnRef(DefId, Vec<TyId>),
    Field {
        base: Box<Expr>,
        index: u32,
        mode: UseMode,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        mode: UseMode,
    },
    /// Struct/class/anon literal; `fields` in declaration order.
    AdtLit {
        def: DefId,
        type_args: Vec<TyId>,
        fields: Vec<Expr>,
    },
    Variant {
        def: DefId,
        type_args: Vec<TyId>,
        variant: u32,
        args: Vec<Expr>,
    },
    ArrayLit(Vec<Expr>),
    Tuple(Vec<Expr>),
    /// Closure creation: `def` is its `Def::Fn`; captures listed in `FnDef::captures`.
    Closure(DefId),
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<Arm>,
    },
    Await(Box<Expr>),
    /// `Some(e)` wrapper inserted by sema when a `T` flows into `T | null`.
    WrapSome(Box<Expr>),
    /// Payload of an Option known to be Some (flow-narrowed); `mode` as for `Field`.
    UnwrapSome(Box<Expr>, UseMode),
    /// Single payload of the enum value `expr` known to be in variant `variant` (a flow-narrowed
    /// union local); a place projection like `UnwrapSome`, `mode` as for `Field`.
    UnwrapVariant {
        expr: Box<Expr>,
        variant: u32,
        mode: UseMode,
    },
    /// `new C<T>(args)` (see header); `args` exclude `this`.
    New {
        def: DefId,
        type_args: Vec<TyId>,
        args: Vec<Expr>,
    },
    /// Subclass instance -> base class (no-op on the pointer); `Expr::ty` is the base type. Also
    /// an object type -> one that differs only in `readonly` fields, which are the same type
    /// once the program is finished (docs/internals/design/shared-models.md).
    Upcast(Box<Expr>),
    /// Concrete value -> interface value via `Program::impls[impl_index]`; the value is moved.
    ToDyn {
        expr: Box<Expr>,
        impl_index: u32,
    },
    /// `throw e` (type Never).
    Throw(Box<Expr>),
}

// ───────────────────────────── Patterns (M2+ draft) ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Pat {
    pub kind: PatKind,
    pub ty: TyId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum PatKind {
    Wildcard,
    Binding(LocalId, UseMode),
    Lit(Lit),
    Variant {
        def: DefId,
        variant: u32,
        args: Vec<Pat>,
    },
    /// Field index → sub-pattern.
    Adt {
        fields: Vec<(u32, Pat)>,
    },
    Tuple(Vec<Pat>),
    Array {
        elems: Vec<Pat>,
        rest: Option<LocalId>,
    },
    Or(Vec<Pat>),
    /// `null` pattern on Option.
    None,
    /// Non-null pattern on Option.
    Some(Box<Pat>),
}
