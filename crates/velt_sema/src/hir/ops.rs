//! HIR literals and operators (moved out of `hir/mod.rs`, unchanged).

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
