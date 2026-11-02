//! Operator methods (design operators.md, #2050): `a + b` on a type with no built-in `+` is
//! `a[Symbol.add](b)`, `-a` is `a[Symbol.neg]()`, `a += b` may be `a[Symbol.addAssign](b)` and
//! `a[i]` is `a[Symbol.index](i)`. This is the table from operators to their member names
//! ([`crate::OPERATOR_SYMBOLS`]); velt_sema checks the declarations and the uses.

use crate::ast::{BinaryOp, UnaryOp};

/// `(operator, method, compound-assignment method)` of the twelve binary operators.
const BINARY: [(BinaryOp, &str, &str); 12] = [
    (BinaryOp::Add, "[Symbol.add]", "[Symbol.addAssign]"),
    (BinaryOp::Sub, "[Symbol.sub]", "[Symbol.subAssign]"),
    (BinaryOp::Mul, "[Symbol.mul]", "[Symbol.mulAssign]"),
    (BinaryOp::Div, "[Symbol.div]", "[Symbol.divAssign]"),
    (BinaryOp::Rem, "[Symbol.rem]", "[Symbol.remAssign]"),
    (BinaryOp::Pow, "[Symbol.pow]", "[Symbol.powAssign]"),
    (BinaryOp::BitAnd, "[Symbol.bitAnd]", "[Symbol.bitAndAssign]"),
    (BinaryOp::BitOr, "[Symbol.bitOr]", "[Symbol.bitOrAssign]"),
    (BinaryOp::BitXor, "[Symbol.bitXor]", "[Symbol.bitXorAssign]"),
    (BinaryOp::Shl, "[Symbol.shl]", "[Symbol.shlAssign]"),
    (BinaryOp::Shr, "[Symbol.shr]", "[Symbol.shrAssign]"),
    (BinaryOp::UShr, "[Symbol.ushr]", "[Symbol.ushrAssign]"),
];

/// The method `-a` calls.
pub const NEG: &str = "[Symbol.neg]";
/// The method `~a` calls.
pub const BIT_NOT: &str = "[Symbol.bitNot]";
/// The method `a[i]` calls on a type with no built-in indexing.
pub const INDEX: &str = "[Symbol.index]";
/// The method `a[i] = v` calls.
pub const SET_INDEX: &str = "[Symbol.setIndex]";

/// The method `a op b` calls (`None`: `op` is not overloadable: comparisons, `in`, `&&`, `||`,
/// `??`).
pub fn binary_method(op: BinaryOp) -> Option<&'static str> {
    BINARY.iter().find(|(o, ..)| *o == op).map(|(_, m, _)| *m)
}

/// The method `a op= b` calls to change `a` in place.
pub fn assign_method(op: BinaryOp) -> Option<&'static str> {
    BINARY.iter().find(|(o, ..)| *o == op).map(|(.., m)| *m)
}

/// The method a unary operator calls (`-` and `~`; `+` is ToNumber, `!` a test).
pub fn unary_method(op: UnaryOp) -> Option<&'static str> {
    match op {
        UnaryOp::Neg => Some(NEG),
        UnaryOp::BitNot => Some(BIT_NOT),
        _ => None,
    }
}

/// What an operator method's name declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpMethod {
    /// `[Symbol.add]` & co.: `(other: R): O`, or `static (l: L, r: Self): O`.
    Binary(BinaryOp),
    /// `[Symbol.neg]`, `[Symbol.bitNot]`: `(): O`.
    Unary(&'static str),
    /// `[Symbol.addAssign]` & co.: `(other: R): void`, changes `this`.
    Assign(BinaryOp),
    /// `[Symbol.index]`: `(i: K, …): V`.
    Index,
    /// `[Symbol.setIndex]`: `(i: K, …, v: V): void`.
    SetIndex,
}

/// The operator a member name declares, if it is one of the operator symbols.
pub fn op_method(name: &str) -> Option<OpMethod> {
    if let Some((op, ..)) = BINARY.iter().find(|(_, m, _)| *m == name) {
        return Some(OpMethod::Binary(*op));
    }
    if let Some((op, ..)) = BINARY.iter().find(|(.., m)| *m == name) {
        return Some(OpMethod::Assign(*op));
    }
    match name {
        NEG => Some(OpMethod::Unary("-")),
        BIT_NOT => Some(OpMethod::Unary("~")),
        INDEX => Some(OpMethod::Index),
        SET_INDEX => Some(OpMethod::SetIndex),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operator_symbol_has_a_row() {
        for name in crate::OPERATOR_SYMBOLS {
            let member = format!("[Symbol.{name}]");
            assert!(op_method(&member).is_some(), "{member}");
        }
        assert_eq!(op_method("[Symbol.iterator]"), None);
        assert_eq!(binary_method(BinaryOp::Lt), None);
        assert_eq!(assign_method(BinaryOp::UShr), Some("[Symbol.ushrAssign]"));
    }
}
