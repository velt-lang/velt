//! Read-only HIR traversal: [`crate::visit`]'s, node for node, over shared references. A pass
//! that only reads bodies walks them with this, so it never takes a body out of `Ctx::defs` to
//! read it (in a check by units, a finished unit's bodies are read and never written:
//! `crate::units`). Keep the two traversals in step: a node one visits, the other visits.

use crate::hir::{Block, Callee, Expr, ExprKind as E, LocalId, Pat, PatKind, Stmt, StmtKind as S};

pub(crate) trait Visit {
    fn stmt(&mut self, _s: &Stmt) {}
    fn expr(&mut self, _e: &Expr) {}
    fn pat(&mut self, _p: &Pat) {}
    fn local(&mut self, _l: LocalId) {}
}

pub(crate) fn block<V: Visit + ?Sized>(b: &Block, v: &mut V) {
    for s in &b.stmts {
        stmt(s, v);
    }
    if let Some(e) = &b.value {
        expr(e, v);
    }
}

pub(crate) fn stmt<V: Visit + ?Sized>(s: &Stmt, v: &mut V) {
    v.stmt(s);
    match &s.kind {
        S::Let { local, init } => {
            v.local(*local);
            if let Some(e) = init {
                expr(e, v);
            }
        }
        S::LetPat { pat: p, init } => {
            pat(p, v);
            expr(init, v);
        }
        S::Expr(e) => expr(e, v),
        S::Return(e) => {
            if let Some(e) = e {
                expr(e, v);
            }
        }
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
        S::ForOf {
            binding,
            iter,
            body,
            ..
        } => {
            pat(binding, v);
            expr(iter, v);
            block(body, v);
        }
        S::Try {
            body,
            catch,
            finally,
        } => {
            block(body, v);
            if let Some((l, b)) = catch {
                if let Some(l) = l {
                    v.local(*l);
                }
                block(b, v);
            }
            if let Some(b) = finally {
                block(b, v);
            }
        }
        S::Break(_) | S::Continue(_) => {}
        S::Block(b) => block(b, v),
    }
}

pub(crate) fn expr<V: Visit + ?Sized>(e: &Expr, v: &mut V) {
    v.expr(e);
    match &e.kind {
        E::Lit(_) | E::ConstArray(_) | E::Global(_) | E::FnRef(..) | E::Closure(_) => {}
        E::Local(l, _) => v.local(*l),
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
        | E::Field { base: x, .. } => expr(x, v),
        E::Binary { lhs, rhs, .. } | E::Logical { lhs, rhs, .. } => {
            expr(lhs, v);
            expr(rhs, v);
        }
        E::Assign { place, value } | E::CompoundAssign { place, value, .. } => {
            expr(place, v);
            expr(value, v);
        }
        E::Index { base, index, .. } => {
            expr(base, v);
            expr(index, v);
        }
        E::Call { callee, args } => {
            if let Callee::Indirect(c) = callee {
                expr(c, v);
            }
            args.iter().for_each(|a| expr(a, v));
        }
        E::If { cond, then, els } => {
            expr(cond, v);
            expr(then, v);
            expr(els, v);
        }
        E::Block(b) => block(b, v),
        E::AdtLit { fields: xs, .. }
        | E::Variant { args: xs, .. }
        | E::ArrayLit(xs)
        | E::Tuple(xs)
        | E::New { args: xs, .. } => xs.iter().for_each(|a| expr(a, v)),
        E::Match { scrutinee, arms } => {
            expr(scrutinee, v);
            for a in arms {
                pat(&a.pat, v);
                if let Some(g) = &a.guard {
                    expr(g, v);
                }
                expr(&a.body, v);
            }
        }
    }
}

pub(crate) fn pat<V: Visit + ?Sized>(p: &Pat, v: &mut V) {
    v.pat(p);
    match &p.kind {
        PatKind::Binding(l, _) => v.local(*l),
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter().for_each(|x| pat(x, v))
        }
        PatKind::Array { elems, rest } => {
            elems.iter().for_each(|x| pat(x, v));
            if let Some(l) = rest {
                v.local(*l);
            }
        }
        PatKind::Adt { fields } => fields.iter().for_each(|(_, x)| pat(x, v)),
        PatKind::Some(x) => pat(x, v),
        PatKind::Wildcard | PatKind::Lit(_) | PatKind::None | PatKind::InstanceOf(_) => {}
    }
}

/// Calls `f` on every expression of `b` (pre-order, nested blocks included).
pub(crate) fn exprs(b: &Block, f: &mut dyn FnMut(&Expr)) {
    struct Exprs<'a>(&'a mut dyn FnMut(&Expr));
    impl Visit for Exprs<'_> {
        fn expr(&mut self, e: &Expr) {
            (self.0)(e);
        }
    }
    block(b, &mut Exprs(f));
}

/// Calls `f` on every expression of `e` (pre-order, `e` included).
pub(crate) fn exprs_in(e: &Expr, f: &mut dyn FnMut(&Expr)) {
    struct Exprs<'a>(&'a mut dyn FnMut(&Expr));
    impl Visit for Exprs<'_> {
        fn expr(&mut self, e: &Expr) {
            (self.0)(e);
        }
    }
    expr(e, &mut Exprs(f));
}

/// The `if` statements of `b` whose condition is `g(x) && MayHoldPrimitive(x)` (a type
/// predicate on a value of a type parameter's type, hir_encodings.md "Type predicates on a type
/// parameter"): for every expression in the `then` block of one, a bit per type parameter
/// (`param_of` of the probe's type) whose instantiation with an object type drops it.
/// Expressions are keyed by address: the caller walks `b` again, unchanged.
pub(crate) fn primitive_guards(
    b: &Block,
    param_of: &dyn Fn(crate::hir::TyId) -> Option<u32>,
) -> rustc_hash::FxHashMap<*const Expr, u64> {
    struct Guards<'a> {
        param_of: &'a dyn Fn(crate::hir::TyId) -> Option<u32>,
        out: rustc_hash::FxHashMap<*const Expr, u64>,
    }
    impl Visit for Guards<'_> {
        fn stmt(&mut self, s: &Stmt) {
            let S::If { cond, then, .. } = &s.kind else {
                return;
            };
            let p = crate::visit::guard_param(cond).and_then(|t| (self.param_of)(t));
            let Some(bit) = p.and_then(|p| 1u64.checked_shl(p)) else {
                return;
            };
            let out = &mut self.out;
            exprs(then, &mut |e: &Expr| {
                *out.entry(e as *const Expr).or_default() |= bit;
            });
        }
    }
    let mut g = Guards {
        param_of,
        out: Default::default(),
    };
    block(b, &mut g);
    g.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hir::{Lit, TyId};
    use velt_common::Span;

    fn lit(n: i64) -> Expr {
        Expr {
            kind: E::Lit(Lit::Int(n as u128)),
            ty: TyId(0),
            span: Span::DUMMY,
        }
    }

    /// Counts what each traversal visits.
    #[derive(Default)]
    struct Count(usize, usize, usize, usize);
    impl Visit for Count {
        fn stmt(&mut self, _: &Stmt) {
            self.0 += 1;
        }
        fn expr(&mut self, _: &Expr) {
            self.1 += 1;
        }
        fn pat(&mut self, _: &Pat) {
            self.2 += 1;
        }
        fn local(&mut self, _: LocalId) {
            self.3 += 1;
        }
    }
    impl crate::visit::VisitMut for Count {
        fn stmt(&mut self, _: &mut Stmt) {
            self.0 += 1;
        }
        fn expr(&mut self, _: &mut Expr) {
            self.1 += 1;
        }
        fn pat(&mut self, _: &mut Pat) {
            self.2 += 1;
        }
        fn local(&mut self, _: &mut LocalId) {
            self.3 += 1;
        }
    }

    #[test]
    fn the_read_only_traversal_visits_what_the_mutable_one_does() {
        let local = |n: u32| Expr {
            kind: E::Local(LocalId(n), crate::hir::UseMode::Borrow),
            ty: TyId(0),
            span: Span::DUMMY,
        };
        let mut b = Block {
            stmts: vec![
                Stmt {
                    kind: S::Let {
                        local: LocalId(0),
                        init: Some(lit(1)),
                    },
                    span: Span::DUMMY,
                },
                Stmt {
                    kind: S::Expr(Expr {
                        kind: E::Binary {
                            op: crate::hir::BinOp::Add,
                            lhs: Box::new(local(0)),
                            rhs: Box::new(lit(2)),
                        },
                        ty: TyId(0),
                        span: Span::DUMMY,
                    }),
                    span: Span::DUMMY,
                },
            ],
            value: Some(Box::new(local(0))),
            span: Span::DUMMY,
        };
        let mut read = Count::default();
        block(&b, &mut read);
        let mut write = Count::default();
        crate::visit::block(&mut b, &mut write);
        assert_eq!(
            (read.0, read.1, read.2, read.3),
            (write.0, write.1, write.2, write.3)
        );
        assert_eq!((read.0, read.1, read.3), (2, 5, 3));
    }
}
