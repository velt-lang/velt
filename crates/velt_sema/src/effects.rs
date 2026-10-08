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

#[cfg(test)]
mod tests {
    use super::may_change_memory;
    use crate::hir::UseMode;
    use crate::hir::{BinOp, Callee, DefId, Expr, ExprKind as H, Intrinsic, Lit, LocalId, TyId};
    use velt_common::Span;

    fn mk(kind: H) -> Expr {
        Expr {
            kind,
            ty: TyId(0),
            span: Span::default(),
        }
    }

    fn local(mode: UseMode) -> Expr {
        mk(H::Local(LocalId(0), mode))
    }

    /// `xs[0]`, read with `mode`.
    fn element(mode: UseMode) -> Expr {
        mk(H::Index {
            base: Box::new(local(UseMode::Borrow)),
            index: Box::new(mk(H::Lit(Lit::Int(0)))),
            mode,
        })
    }

    fn intrinsic(i: Intrinsic, args: Vec<Expr>) -> Expr {
        mk(H::Call {
            callee: Callee::Intrinsic(i),
            args,
        })
    }

    #[test]
    fn reads_and_arithmetic_change_nothing() {
        let sum = mk(H::Binary {
            op: BinOp::Add,
            lhs: Box::new(element(UseMode::Copy)),
            rhs: Box::new(mk(H::Lit(Lit::Float(1.0)))),
        });
        assert!(!may_change_memory(&sum));
        assert!(!may_change_memory(&intrinsic(
            Intrinsic::ArrayLen,
            vec![local(UseMode::Borrow)]
        )));
    }

    #[test]
    fn an_intrinsic_handed_a_place_to_modify_changes_memory() {
        // `xs.pop()`: the receiver is borrowed mutably.
        let pop = intrinsic(Intrinsic::ArrayPop, vec![local(UseMode::BorrowMut)]);
        assert!(may_change_memory(&pop));
        // Moving an element out of its place changes the place too.
        let moved = intrinsic(Intrinsic::ArrayLen, vec![element(UseMode::Move)]);
        assert!(may_change_memory(&moved));
    }

    #[test]
    fn a_call_to_a_function_or_getter_changes_memory() {
        // A getter is a call of its `Def::Fn` (`p.x` with `get x()`).
        let getter = mk(H::Call {
            callee: Callee::Def(DefId(1), vec![]),
            args: vec![local(UseMode::Borrow)],
        });
        assert!(may_change_memory(&getter));
        // An argument that runs code counts inside an intrinsic as well.
        let len = intrinsic(Intrinsic::ArrayLen, vec![getter]);
        assert!(may_change_memory(&len));
    }

    #[test]
    fn closures_and_await_change_memory() {
        let through_value = mk(H::Call {
            callee: Callee::Indirect(Box::new(local(UseMode::Borrow))),
            args: vec![],
        });
        assert!(may_change_memory(&through_value));
        // Creating a closure may move its captures: conservative.
        assert!(may_change_memory(&mk(H::Closure(DefId(2)))));
        assert!(may_change_memory(&mk(H::Await(Box::new(local(
            UseMode::Move
        ))))));
    }
}
