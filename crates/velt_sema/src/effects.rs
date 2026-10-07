//! What evaluating an expression may do to memory, for the stages that order a read of a place
//! against a later operand: sema hoists the current value of `rows[0].out += f()`, VIR lowering
//! forms an element's address again after `xs[0] += grow(xs)` (#580). One predicate, so both
//! stages agree on which right-hand sides need it.

use crate::hir::{self, ExprKind as H, UseMode};

/// Can evaluating `e` run code that changes memory: an assignment, a call that is not an
/// intrinsic, `new`, `await`, or an intrinsic that is handed a place to modify or move
/// (`xs.pop()`)? Then an address into an array computed before `e` may be stale after it.
/// Conservative: anything else but reads, literals and arithmetic counts.
pub fn may_change_memory(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Lit(_) | H::Local(..) | H::Global(_) | H::FnRef(..) => false,
        H::Unary { expr: x, .. }
        | H::Cast(x)
        | H::WrapSome(x)
        | H::Upcast(x)
        | H::Downcast(x)
        | H::UnwrapSome(x, _)
        | H::UnwrapVariant { expr: x, .. }
        | H::Field { base: x, .. } => may_change_memory(x),
        H::Index { base, index, .. } => may_change_memory(base) || may_change_memory(index),
        H::Binary { lhs, rhs, .. } | H::Logical { lhs, rhs, .. } => {
            may_change_memory(lhs) || may_change_memory(rhs)
        }
        H::If { cond, then, els } => {
            may_change_memory(cond) || may_change_memory(then) || may_change_memory(els)
        }
        H::Call {
            callee: hir::Callee::Intrinsic(_),
            args,
        } => args.iter().any(|a| may_change_memory(a) || writes_place(a)),
        _ => true,
    }
}

/// Is `a` a place passed to modify or move it (an intrinsic argument like `pop`'s receiver)?
fn writes_place(a: &hir::Expr) -> bool {
    let mode = match &a.kind {
        H::Local(_, m) | H::Field { mode: m, .. } | H::Index { mode: m, .. } => *m,
        H::UnwrapSome(_, m) | H::UnwrapVariant { mode: m, .. } => *m,
        _ => return false,
    };
    matches!(mode, UseMode::BorrowMut | UseMode::Move)
}
