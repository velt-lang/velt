//! Assignment targets that can be read again without an effect (`assign.rs`).

use velt_syntax::ast;

use crate::body::places::is_place;
use crate::body::Want;
use crate::hir::{self, ExprKind as H, TyKind};

/// `x`, `this.a`, `x.a.b`: re-reading it has no side effects.
/// A place that reading again has no effect: names, `this`, member paths, and indexes by such
/// values, literals and arithmetic on them (`xs[i + 1].count`).
pub(super) fn is_pure_place(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Index {
            object,
            index,
            optional: false,
        } => is_pure_place(object) && is_pure_value(index),
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_pure_place(object),
        ast::ExprKind::Paren(inner) => is_pure_place(inner),
        _ => is_plain_path(e),
    }
}

pub(super) fn is_pure_value(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Lit(_) => true,
        ast::ExprKind::Unary { expr, .. } => is_pure_value(expr),
        ast::ExprKind::Binary { lhs, rhs, .. } => is_pure_value(lhs) && is_pure_value(rhs),
        _ => is_pure_place(e),
    }
}

pub(super) fn is_plain_path(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        } => is_plain_path(object),
        ast::ExprKind::Paren(inner) => is_plain_path(inner),
        _ => false,
    }
}

impl crate::body::FnCx<'_, '_> {
    /// The object of a member or element assignment target, checked. `p!` (or `p as T`, `T` the
    /// type `p` holds when present) on a place `p` that holds a tuple (`ht.t![0] = 3`,
    /// `ys[i++]![1] = "q"`) is the place holding `p`'s value, so the tuple is written in place,
    /// not in a copy. The indexes in `p` that have an effect are bound once and the null check
    /// runs, as hidden statements before the assignment (`FnCx::call_temps`).
    pub(super) fn assign_receiver(&mut self, object: &ast::Expr) -> hir::Expr {
        let b = strip_parens(object);
        let (inner, cast) = match &b.kind {
            ast::ExprKind::NonNull(inner) => (inner, None),
            ast::ExprKind::Cast { expr, ty } => (expr, Some(ty)),
            _ => return self.expr(object, None, Want::Borrow),
        };
        if !held_shape(inner) {
            return self.expr(object, None, Want::Borrow);
        }
        let inner = &self.bind_effects(inner);
        let object = &ast::Expr {
            id: b.id,
            kind: match cast {
                None => ast::ExprKind::NonNull(Box::new(inner.clone())),
                Some(ty) => ast::ExprKind::Cast {
                    expr: Box::new(inner.clone()),
                    ty: ty.clone(),
                },
            },
            span: b.span,
        };
        let mark = self.cx.diags.len();
        let p = self.expr(inner, None, Want::Borrow);
        // A variable a closure assigns is read through a check of its narrowing: its place is
        // the variable itself.
        let p = match p.kind {
            H::Match { scrutinee, .. }
                if is_place(&scrutinee) && self.cx.ty.opt_payload(scrutinee.ty) == Some(p.ty) =>
            {
                *scrutinee
            }
            _ => p,
        };
        let payload = self.cx.ty.opt_payload(p.ty);
        // `p as T` names `p`'s place only when `T` is what `p` holds when present.
        let same = match (cast, payload) {
            (Some(ty), Some(payload)) => self.resolve(ty) == payload,
            _ => true,
        };
        // Only a tuple needs its place: an object the check gives is the object itself, which a
        // later operand replacing `p` must not change (`B!.n += clearB()`).
        let tuple = payload.is_some_and(|t| matches!(self.cx.ty.kind(t), TyKind::Tuple(_)));
        if let (Some(payload), true, true, true) = (payload, same, tuple, is_place(&p)) {
            if self.cx.dynamic(p.ty).is_none() {
                let check = self.expr(object, None, Want::Borrow);
                let span = check.span;
                self.call_temps.push(hir::Stmt {
                    kind: hir::StmtKind::Expr(check),
                    span,
                });
                let h = H::UnwrapSome(Box::new(p), hir::UseMode::Borrow);
                return self.mk(h, payload, object.span);
            }
        }
        self.cx.diags.truncate(mark);
        self.expr(object, None, Want::Borrow)
    }

    /// `e` (a [`held_shape`]) with each index that has an effect bound to a hidden temporary,
    /// in evaluation order.
    fn bind_effects(&mut self, e: &ast::Expr) -> ast::Expr {
        let kind = match &strip_parens(e).kind {
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } => ast::ExprKind::Member {
                object: Box::new(self.bind_effects(object)),
                prop: prop.clone(),
                optional: false,
            },
            ast::ExprKind::Index {
                object,
                index,
                optional: false,
            } => {
                let object = Box::new(self.bind_effects(object));
                let index = match is_pure_value(index) {
                    true => index.clone(),
                    false => {
                        let mut lets = vec![];
                        let i = self.bind_hoisted(index, &mut lets);
                        self.call_temps.append(&mut lets);
                        Box::new(i)
                    }
                };
                ast::ExprKind::Index {
                    object,
                    index,
                    optional: false,
                }
            }
            _ => return e.clone(),
        };
        ast::Expr {
            id: e.id,
            kind,
            span: e.span,
        }
    }
}

fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => strip_parens(x),
        _ => e,
    }
}

/// A variable or `this` with members and elements (at any index) taken: a place, once its
/// indexes are bound.
fn held_shape(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Paren(x) => held_shape(x),
        ast::ExprKind::Member {
            object,
            optional: false,
            ..
        }
        | ast::ExprKind::Index {
            object,
            optional: false,
            ..
        } => held_shape(object),
        _ => false,
    }
}
