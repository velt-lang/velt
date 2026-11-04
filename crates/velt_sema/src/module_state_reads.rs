//! The module globals an initializer reads whatever happens at run time: not those in a branch
//! that may not be taken (`c ? x : y`, `a && x`, a `match` arm, a loop's body), nor in a closure,
//! which runs when it is called. A read that only may run isn't an error: if it does run before
//! the global's initializer, it stops with the `ReferenceError` at run time.

use velt_common::Span;

use crate::hir::{self, Callee, ExprKind as E, StmtKind as S};

/// Every module global `e` reads on all its paths.
pub(super) fn reads(e: &hir::Expr, f: &mut dyn FnMut(hir::DefId, Span)) {
    match &e.kind {
        E::Global(d) => f(*d, e.span),
        E::Lit(_) | E::FnRef(..) | E::Closure(_) | E::Local(..) => {}
        E::Unary { expr: x, .. }
        | E::Cast(x)
        | E::Await(x)
        | E::WrapSome(x)
        | E::UnwrapSome(x, _)
        | E::UnwrapVariant { expr: x, .. }
        | E::Upcast(x)
        | E::Downcast(x)
        | E::ToDyn { expr: x, .. }
        | E::Throw(x)
        | E::Field { base: x, .. } => reads(x, f),
        E::Binary { lhs, rhs, .. } => {
            reads(lhs, f);
            reads(rhs, f);
        }
        E::Logical { lhs, .. } => reads(lhs, f),
        E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
            reads(place, f);
            reads(value, f);
        }
        E::Index { base, index, .. } => {
            reads(base, f);
            reads(index, f);
        }
        E::Call { callee, args } => {
            if let Callee::Indirect(c) = callee {
                reads(c, f);
            }
            args.iter().for_each(|a| reads(a, f));
        }
        E::If { cond, .. } => reads(cond, f),
        E::Block(b) => block(b, f),
        E::AdtLit { fields: xs, .. }
        | E::Variant { args: xs, .. }
        | E::ArrayLit(xs)
        | E::Tuple(xs)
        | E::New { args: xs, .. } => xs.iter().for_each(|a| reads(a, f)),
        E::Match { scrutinee, .. } => reads(scrutinee, f),
    }
}

/// The statements of `b` up to the first that may leave it or may not run the rest.
fn block(b: &hir::Block, f: &mut dyn FnMut(hir::DefId, Span)) {
    for s in &b.stmts {
        match &s.kind {
            S::Let { init, .. } => init.iter().for_each(|e| reads(e, f)),
            S::LetPat { init, .. } => reads(init, f),
            S::Expr(e) => reads(e, f),
            S::If { cond, .. } => {
                reads(cond, f);
                return;
            }
            S::Block(inner) => block(inner, f),
            _ => return,
        }
    }
    if let Some(e) = &b.value {
        reads(e, f);
    }
}
