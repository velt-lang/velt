//! Assignment targets that can be read again without an effect (`assign.rs`).

use velt_syntax::ast;

use crate::body::places::is_place;
use crate::body::{LocalKind, Want};
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

/// How the tuple a write behind `p!` / `p as T` lands in is checked again after a right-hand
/// side with effects ([`crate::body::FnCx::held_rechecked`]).
pub(crate) enum HeldRecheck {
    /// `p` (an option place) is still present.
    Present(hir::Expr),
    /// `p as T` (`p` a union place, `T` its tuple member) still holds a `T`: the assertion,
    /// checked again.
    Assert(ast::Expr),
}

impl crate::body::FnCx<'_, '_> {
    /// The object of a member or element assignment target, checked. `p!` (or `p as T`, `T` the
    /// type `p` holds when present) on a place `p` that holds a tuple (`ht.t![0] = 3`,
    /// `ys[i++]![1] = "q"`) is the place holding `p`'s value, so the tuple is written in place,
    /// not in a copy; so is `p as T` on a union place `p` with the tuple member `T`. A root of
    /// `p` that is not a variable (`(c ? a : b).t!`) and the indexes in `p` that have an effect
    /// (every index that is not a literal, when one has an effect or the place holds a tuple) are
    /// bound once, and the null check or assertion runs, as hidden statements before the
    /// assignment (`FnCx::call_temps`).
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
        // One list for every temporary of this target, so each gets its own name.
        let mut lets = vec![];
        let all = !pure_indexes(inner);
        let rebuild = |inner: &ast::Expr| ast::Expr {
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
        let mut inner = self.bind_effects(inner, all, &mut lets);
        let mut object = rebuild(&inner);
        let mark = self.cx.diags.len();
        let mut p = self.held_place(&inner);
        let held = self.held_tuple(&p, cast) || self.held_member(&p, cast).is_some();
        if !all && held && has_path_index(&inner) {
            // The check after a right-hand side that may change memory reads the place again:
            // an index that is a variable or a path is bound too, so the check reads the slot
            // written (`ws[k]![0] = (() => { k = 1; return 5; })()`).
            self.cx.diags.truncate(mark);
            inner = self.bind_effects(&inner, true, &mut lets);
            object = rebuild(&inner);
            p = self.held_place(&inner);
        }
        self.call_temps.append(&mut lets);
        let object = &object;
        if held {
            self.note_held_write(&inner, &p);
        }
        if self.held_tuple(&p, cast) {
            if let Some(payload) = self.cx.ty.opt_payload(p.ty) {
                let check = self.expr(object, None, Want::Borrow);
                let span = check.span;
                self.call_temps.push(hir::Stmt {
                    kind: hir::StmtKind::Expr(check),
                    span,
                });
                self.held_recheck = Some(HeldRecheck::Present(p.clone()));
                let h = H::UnwrapSome(Box::new(p), hir::UseMode::Borrow);
                return self.mk(h, payload, object.span);
            }
        }
        if let Some((variant, member)) = self.held_member(&p, cast) {
            // `(u as [number, string])[0] = 9` on a union place: the member's place, after the
            // assertion (which a union local's own projection does not check again).
            let check = self.expr(object, None, Want::Borrow);
            let span = check.span;
            self.call_temps.push(hir::Stmt {
                kind: hir::StmtKind::Expr(check),
                span,
            });
            self.held_recheck = Some(HeldRecheck::Assert(object.clone()));
            let h = H::UnwrapVariant {
                expr: Box::new(p),
                variant,
                mode: hir::UseMode::Borrow,
            };
            return self.mk(h, member, object.span);
        }
        self.cx.diags.truncate(mark);
        self.expr(object, None, Want::Borrow)
    }

    /// `f` checking an assignment: the target's hidden statements (`assign_receiver`) are
    /// returned to run before it, and a tuple place to check again stays within it.
    pub(super) fn target_scope<R>(
        &mut self,
        f: impl FnOnce(&mut Self) -> R,
    ) -> (R, Vec<hir::Stmt>) {
        let outer = std::mem::take(&mut self.call_temps);
        let recheck = self.held_recheck.take();
        let r = f(self);
        self.held_recheck = recheck;
        (r, std::mem::replace(&mut self.call_temps, outer))
    }

    /// The value `v` written into a tuple behind `p!` / `p as T` (`assign_receiver`), then a
    /// check that `v` left `p` present: a right-hand side that sets `p` to null
    /// (`ht.t![0] = (() => { ht.t = null; return 5; })()`) stops the program with a message
    /// naming that, where Node writes into the tuple `p` held before (#1352).
    pub(super) fn held_rechecked(&mut self, v: hir::Expr) -> hir::Expr {
        let Some(recheck) = self.held_recheck.take() else {
            return v;
        };
        if !crate::effects::may_change_memory(&v) {
            return v;
        }
        let (ty, span) = (v.ty, v.span);
        let tmp = self.new_local("<value>", ty, false, span, LocalKind::Temp);
        // The right-hand side, or another task during its `await`, cleared the place.
        let msg =
            "the tuple this assignment writes into was set to null while its right-hand side ran";
        let check = match recheck {
            HeldRecheck::Present(p) => self.checked_present(p, msg, None, true, span),
            HeldRecheck::Assert(object) => self.expr(&object, None, Want::Borrow),
        };
        let stmts = vec![
            hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(v),
                },
                span,
            },
            hir::Stmt {
                kind: hir::StmtKind::Expr(check),
                span,
            },
        ];
        let value = self.mk(H::Local(tmp, hir::UseMode::Move), ty, span);
        self.with_temps_value(stmts, value)
    }

    /// `p` (the checked `inner` of a held target) as the place that holds the tuple: a variable
    /// a closure assigns is read through a check of its narrowing, and its place is the variable
    /// itself.
    fn held_place(&mut self, inner: &ast::Expr) -> hir::Expr {
        let p = self.expr(inner, None, Want::Borrow);
        match p.kind {
            H::Match { scrutinee, .. }
                if is_place(&scrutinee) && self.cx.ty.opt_payload(scrutinee.ty) == Some(p.ty) =>
            {
                *scrutinee
            }
            _ => p,
        }
    }

    /// Is `p` (`p as cast` when given) a place that holds a tuple when present? `p as T` names
    /// `p`'s place only when `T` is what `p` holds when present. Only a tuple needs its place:
    /// an object the check gives is the object itself, which a later operand replacing `p` must
    /// not change (`B!.n += clearB()`).
    fn held_tuple(&mut self, p: &hir::Expr, cast: Option<&ast::TypeExpr>) -> bool {
        let Some(payload) = self.cx.ty.opt_payload(p.ty) else {
            return false;
        };
        let same = match cast {
            Some(ty) => self.resolve(ty) == payload,
            None => true,
        };
        same && matches!(self.cx.ty.kind(payload), TyKind::Tuple(_))
            && is_place(p)
            && self.cx.dynamic(p.ty).is_none()
    }

    /// A write into the tuple a field `o.t` holds is a write of the field, for the copies of
    /// `crate::object_copies` (as `o.t = v` is): `o`'s object stays the one the program holds.
    fn note_held_write(&mut self, inner: &ast::Expr, p: &hir::Expr) {
        if let (ast::ExprKind::Member { prop, .. }, H::Field { base, .. }) =
            (&strip_parens(inner).kind, &p.kind)
        {
            let ty = base.ty;
            self.note_field_write(ty, prop);
        }
    }

    /// `(variant, T)` when `p` is a union place and `cast` its tuple member `T`.
    fn held_member(
        &mut self,
        p: &hir::Expr,
        cast: Option<&ast::TypeExpr>,
    ) -> Option<(u32, hir::TyId)> {
        let target = self.resolve(cast?);
        if !matches!(self.cx.ty.kind(target), TyKind::Tuple(_))
            || !is_place(p)
            || self.cx.dynamic(p.ty).is_some()
        {
            return None;
        }
        let members = self.cx.union_members(p.ty)?;
        let v = members.iter().position(|&m| m == target)?;
        Some((v as u32, target))
    }

    /// `e` (a [`held_shape`]) with its root bound to a hidden temporary when it is not a
    /// variable or `this` (`(c ? a : b).t`, `ps.find(…)!.t`: the object it gives is shared, so
    /// the write goes through its place), and each index that has an effect (`all`: each index
    /// that is not a literal) bound too, in evaluation order. The bindings are appended to
    /// `lets`, which names each one apart.
    fn bind_effects(&mut self, e: &ast::Expr, all: bool, lets: &mut Vec<hir::Stmt>) -> ast::Expr {
        let kind = match &strip_parens(e).kind {
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } => ast::ExprKind::Member {
                object: Box::new(self.bind_root(object, all, lets)),
                prop: prop.clone(),
                optional: false,
            },
            ast::ExprKind::Index {
                object,
                index,
                optional: false,
            } => {
                let object = Box::new(self.bind_root(object, all, lets));
                let keep = match all {
                    true => is_literal(index) || is_hoisted(index),
                    false => is_pure_value(index),
                };
                let index = match keep {
                    true => index.clone(),
                    false => Box::new(self.bind_hoisted(index, lets)),
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

    /// [`Self::bind_effects`] of the object of a member or element: a root that is not a
    /// variable or `this` is bound itself.
    /// `p!` and `p as T` keep their form around `p` bound: they name the place `p` names, and a
    /// tuple there stays the one written (`a![1]![0]`).
    fn bind_root(&mut self, e: &ast::Expr, all: bool, lets: &mut Vec<hir::Stmt>) -> ast::Expr {
        let b = strip_parens(e);
        let kind = match &b.kind {
            ast::ExprKind::NonNull(x) => {
                ast::ExprKind::NonNull(Box::new(self.bind_root(x, all, lets)))
            }
            ast::ExprKind::Cast { expr, ty } => ast::ExprKind::Cast {
                expr: Box::new(self.bind_root(expr, all, lets)),
                ty: ty.clone(),
            },
            ast::ExprKind::Super => return e.clone(),
            _ if is_root(b) && !is_plain_path(b) && !is_hoisted(b) => {
                return self.bind_hoisted(e, lets)
            }
            _ => return self.bind_effects(e, all, lets),
        };
        ast::Expr {
            id: b.id,
            kind,
            span: b.span,
        }
    }
}

fn strip_parens(e: &ast::Expr) -> &ast::Expr {
    match &e.kind {
        ast::ExprKind::Paren(x) => strip_parens(x),
        _ => e,
    }
}

/// Is `e` the root of a held target (not a member or an element)?
fn is_root(e: &ast::Expr) -> bool {
    !matches!(
        &strip_parens(e).kind,
        ast::ExprKind::Member { .. } | ast::ExprKind::Index { .. }
    )
}

/// A temporary already bound for this target.
fn is_hoisted(e: &ast::Expr) -> bool {
    matches!(&strip_parens(e).kind, ast::ExprKind::Ident(id) if id.name.starts_with('<'))
}

fn is_literal(e: &ast::Expr) -> bool {
    matches!(&strip_parens(e).kind, ast::ExprKind::Lit(_))
        || super::member_keys::literal_key(e).is_some()
}

/// Are the indexes of the held target `e` free of effects?
fn pure_indexes(e: &ast::Expr) -> bool {
    match &strip_parens(e).kind {
        ast::ExprKind::Member { object, .. } => pure_indexes(object),
        ast::ExprKind::Index { object, index, .. } => is_pure_value(index) && pure_indexes(object),
        _ => true,
    }
}

/// Has the held target `e` an index that is not a literal or a bound temporary?
fn has_path_index(e: &ast::Expr) -> bool {
    match &strip_parens(e).kind {
        ast::ExprKind::Member { object, .. } => has_path_index(object),
        ast::ExprKind::Index { object, index, .. } => {
            !(is_literal(index) || is_hoisted(index)) || has_path_index(object)
        }
        _ => false,
    }
}

/// A variable or `this`, or any other expression with a member or an element taken, with
/// members and elements (at any index) taken: a place, once its root and indexes are bound.
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
        } => is_root(object) || held_shape(object),
        _ => false,
    }
}
