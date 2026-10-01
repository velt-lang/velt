//! Generic mutable HIR traversal used by several passes (type substitution, local renumbering,
//! ownership patching). Visits every expression (pre-order), every pattern node and every
//! local-id occurrence, including inside block expressions and match arms.

use crate::hir::{
    Block, Callee, Expr, ExprKind as E, LocalId, Pat, PatKind, Stmt, StmtKind as S, TyId,
};

pub(crate) trait VisitMut {
    fn stmt(&mut self, _s: &mut Stmt) {}
    fn expr(&mut self, _e: &mut Expr) {}
    fn pat(&mut self, _p: &mut Pat) {}
    fn local(&mut self, _l: &mut LocalId) {}
}

pub(crate) fn block<V: VisitMut + ?Sized>(b: &mut Block, v: &mut V) {
    for s in &mut b.stmts {
        stmt(s, v);
    }
    if let Some(e) = &mut b.value {
        expr(e, v);
    }
}

pub(crate) fn stmt<V: VisitMut + ?Sized>(s: &mut Stmt, v: &mut V) {
    v.stmt(s);
    match &mut s.kind {
        S::Let { local, init } => {
            v.local(local);
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
                    v.local(l);
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

pub(crate) fn expr<V: VisitMut + ?Sized>(e: &mut Expr, v: &mut V) {
    v.expr(e);
    match &mut e.kind {
        E::Lit(_) | E::Global(_) | E::FnRef(..) | E::Closure(_) => {}
        E::Local(l, _) => v.local(l),
        E::Unary { expr: x, .. }
        | E::Cast(x)
        | E::Await(x)
        | E::WrapSome(x)
        | E::UnwrapSome(x, _)
        | E::UnwrapVariant { expr: x, .. }
        | E::Upcast(x)
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
            args.iter_mut().for_each(|a| expr(a, v));
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
        | E::New { args: xs, .. } => xs.iter_mut().for_each(|a| expr(a, v)),
        E::Match { scrutinee, arms } => {
            expr(scrutinee, v);
            for a in arms.iter_mut() {
                pat(&mut a.pat, v);
                if let Some(g) = &mut a.guard {
                    expr(g, v);
                }
                expr(&mut a.body, v);
            }
        }
    }
}

pub(crate) fn pat<V: VisitMut + ?Sized>(p: &mut Pat, v: &mut V) {
    v.pat(p);
    match &mut p.kind {
        PatKind::Binding(l, _) => v.local(l),
        PatKind::Variant { args: ps, .. } | PatKind::Tuple(ps) | PatKind::Or(ps) => {
            ps.iter_mut().for_each(|x| pat(x, v))
        }
        PatKind::Array { elems, rest } => {
            elems.iter_mut().for_each(|x| pat(x, v));
            if let Some(l) = rest {
                v.local(l);
            }
        }
        PatKind::Adt { fields } => fields.iter_mut().for_each(|(_, x)| pat(x, v)),
        PatKind::Some(x) => pat(x, v),
        PatKind::Wildcard | PatKind::Lit(_) | PatKind::None => {}
    }
}

struct TypeMap<'a>(&'a mut dyn FnMut(TyId) -> TyId);

impl VisitMut for TypeMap<'_> {
    fn expr(&mut self, x: &mut Expr) {
        x.ty = (self.0)(x.ty);
        match &mut x.kind {
            E::Call {
                callee: Callee::Def(_, ts),
                ..
            }
            | E::FnRef(_, ts)
            | E::AdtLit { type_args: ts, .. }
            | E::Variant { type_args: ts, .. }
            | E::New { type_args: ts, .. } => ts.iter_mut().for_each(|t| *t = (self.0)(*t)),
            E::Call {
                callee:
                    Callee::ParamMethod {
                        iface_args,
                        method_type_args,
                        ..
                    },
                ..
            } => iface_args
                .iter_mut()
                .chain(method_type_args.iter_mut())
                .for_each(|t| *t = (self.0)(*t)),
            _ => {}
        }
    }

    fn pat(&mut self, p: &mut Pat) {
        p.ty = (self.0)(p.ty);
    }
}

/// Apply `m` to every type in an expression tree (expression and pattern types, type args).
pub(crate) fn map_expr_types(e: &mut Expr, m: &mut dyn FnMut(TyId) -> TyId) {
    expr(e, &mut TypeMap(m));
}

struct Remap<'a>(&'a [LocalId]);

impl VisitMut for Remap<'_> {
    fn local(&mut self, l: &mut LocalId) {
        *l = self.0[l.0 as usize];
    }
}

/// Renumber locals in a function body (`map[old] = new`).
pub(crate) fn remap_locals(b: &mut Block, map: &[LocalId]) {
    block(b, &mut Remap(map));
}

struct Exprs<'a>(&'a mut dyn FnMut(&mut Expr));

impl VisitMut for Exprs<'_> {
    fn expr(&mut self, e: &mut Expr) {
        (self.0)(e);
    }
}

/// Visit every expression of a block (pre-order, including nested blocks and match arms).
pub(crate) fn exprs_mut(b: &mut Block, f: &mut dyn FnMut(&mut Expr)) {
    block(b, &mut Exprs(f));
}
