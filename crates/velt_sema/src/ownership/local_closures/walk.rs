//! Read-only traversal of HIR bodies for this pass and `local_async` (`crate::visit` takes them
//! mutably, and these passes read bodies while the closure definitions are borrowed).

use crate::hir::{Block, Callee, Expr, ExprKind as E, Stmt, StmtKind as S};

/// What a traversal does at each statement and expression (pre-order).
pub(in crate::ownership) trait Visit {
    fn stmt(&mut self, _s: &Stmt) {}
    fn expr(&mut self, _e: &Expr) {}
}

/// Every statement and expression of `b`, nested blocks included.
pub(in crate::ownership) fn block(b: &Block, v: &mut dyn Visit) {
    for s in &b.stmts {
        stmt(s, v);
    }
    if let Some(e) = &b.value {
        expr(e, v);
    }
}

fn stmt(s: &Stmt, v: &mut dyn Visit) {
    v.stmt(s);
    match &s.kind {
        S::Let { init, .. } => {
            if let Some(e) = init {
                expr(e, v);
            }
        }
        S::LetPat { init, .. } | S::Expr(init) | S::Return(Some(init)) => expr(init, v),
        S::Return(None) | S::Break(_) | S::Continue(_) => {}
        S::If { cond, then, els } => {
            expr(cond, v);
            block(then, v);
            if let Some(b) = els {
                block(b, v);
            }
        }
        S::While {
            cond, body, step, ..
        } => {
            expr(cond, v);
            block(body, v);
            if let Some(e) = step {
                expr(e, v);
            }
        }
        S::ForOf { iter, body, .. } => {
            expr(iter, v);
            block(body, v);
        }
        S::Try {
            body,
            catch,
            finally,
        } => {
            block(body, v);
            if let Some((_, b)) = catch {
                block(b, v);
            }
            if let Some(b) = finally {
                block(b, v);
            }
        }
        S::Block(b) => block(b, v),
    }
}

/// `e` and everything inside it.
pub(in crate::ownership) fn expr(e: &Expr, v: &mut dyn Visit) {
    v.expr(e);
    match &e.kind {
        E::Block(b) => block(b, v),
        _ => children(e, &mut |x| expr(x, v)),
    }
}

/// The direct sub-expressions of `e` (a block's statements are left to the caller).
pub(super) fn children(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    match &e.kind {
        E::Lit(_) | E::Global(_) | E::FnRef(..) | E::Closure(_) | E::Local(..) => {}
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
        | E::Field { base: x, .. } => f(x),
        E::Binary { lhs, rhs, .. } | E::Logical { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
            f(place);
            f(value);
        }
        E::Index { base, index, .. } => {
            f(base);
            f(index);
        }
        E::Call { callee, args } => {
            if let Callee::Indirect(c) = callee {
                f(c);
            }
            args.iter().for_each(&mut *f);
        }
        E::If { cond, then, els } => {
            f(cond);
            f(then);
            f(els);
        }
        E::AdtLit { fields: xs, .. }
        | E::Variant { args: xs, .. }
        | E::ArrayLit(xs)
        | E::Tuple(xs)
        | E::New { args: xs, .. } => xs.iter().for_each(&mut *f),
        E::Block(_) => {}
        E::Match { scrutinee, arms } => {
            f(scrutinee);
            for a in arms {
                if let Some(g) = &a.guard {
                    f(g);
                }
                f(&a.body);
            }
        }
    }
}
