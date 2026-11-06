//! Module constants and the module state: the globals of `Program::defs` and the slots that
//! hold module-level `let`s (hir_encodings.md "Module state"). Part of the HIR contract.

use velt_common::Span;

use super::{CTy, DefId, Expr, TyId};

/// Module-level `let`s: one slot each in a process-wide tuple (hir_encodings.md "Module state").
#[derive(Clone, Debug)]
pub struct ModuleState {
    /// The state tuple: element `i` is the type of slot `i`.
    pub ty: TyId,
    /// The global of each slot, in initialization order (Node's module evaluation order).
    pub slots: Vec<DefId>,
    /// The first slot of the root file's declarations, which the entry `main` initializes where
    /// they stand among its statements (`Intrinsic::InitSlots`, #1438); `slots.len()` when the
    /// state's initializer initializes every slot. Additive.
    pub deferred: u32,
}

#[derive(Clone, Debug)]
pub struct GlobalDef {
    pub name: String,
    pub ty: TyId,
    /// Evaluated at each use (a constant expression, or pure calls and closures, sema
    /// `body::pure_init`), except a table: an array literal that lowering stores once as data,
    /// and a module-level `let` (`slot`), which the module state's initializer evaluates once.
    pub init: Expr,
    pub span: Span,
    /// The slot of a module-level `let` in `Program::module_state`: `Global(d)` is then a place
    /// that is read, written and borrowed like a field.
    pub slot: Option<u32>,
    /// `declare c const` (docs/internals/design/c-ffi.md): `init` is a placeholder, and lowering
    /// reads the value the C headers gave. Additive.
    pub c_const: Option<CConst>,
}

/// A C constant: what it crosses as, and its value on the build's target, which the compiler
/// driver sets after sema from the package's C headers or its record of them. `None`: this
/// target has no C headers (WebAssembly), and reading it is an error once lowering is done.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CConst {
    pub ty: CTy,
    pub value: Option<CValue>,
}

/// The value of a C constant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CValue {
    Int(i128),
    /// A `float` is widened.
    Float(f64),
    Bool(bool),
    Address(u64),
}
