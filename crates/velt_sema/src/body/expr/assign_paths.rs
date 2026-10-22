//! Assignment targets (`assign.rs`): those that can be read again without an effect, and
//! writes to readonly fields.

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::{is_place, set_place_mode};
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, ExprKind as H, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    pub(super) fn check_readonly(&mut self, place: &hir::Expr, prop: &ast::Ident) {
        let H::Field { base, index, .. } = &place.kind else {
            return;
        };
        let Some((d, _)) = self.adt_of(base.ty) else {
            return;
        };
        let readonly = self
            .cx
            .adt(d)
            .and_then(|a| a.fields.get(*index as usize))
            .is_some_and(|f| f.readonly);
        if readonly && self.is_import_meta_type(base.ty) {
            // `import.meta.url = …` is a write to the module constant, and so is one through
            // another reference to it (`const m = import.meta; m.url = …`).
            self.cx.err(
                format!(
                    "cannot assign to a field of module-level constant `{}`",
                    ast::IMPORT_META
                ),
                place.span,
            );
            return;
        }
        let in_ctor = self.f.kind == crate::defs::FnKind::Ctor
            && matches!(base.kind, H::Local(l, _) if self.local_kind(l) == LocalKind::This);
        if readonly && !in_ctor {
            self.cx.err(
                format!("cannot assign to `{}`: it is a readonly field", prop.name),
                prop.span,
            );
        }
    }

    /// A write into a part of `target` whose object reaches a module constant evaluated at
    /// each use through `!`, `as`, `??`, `||`, `&&` or `?:` (`K!.v = 1`, `(K as any).v = 1`,
    /// `(c ? K! : d).v = 1`): reported, as `K.v = 1` is, since the write would change a copy.
    /// The source form is followed, whatever the checked target becomes (a conversion to
    /// `any`, a narrowed read).
    pub(super) fn per_use_const_write(&mut self, target: &ast::Expr) -> bool {
        let (object, what) = match &strip_parens(target).kind {
            ast::ExprKind::Member { object, .. } => (object, "assign to a field of"),
            ast::ExprKind::Index { object, .. } => (object, "assign to an element of"),
            _ => return false,
        };
        let Some((name, span)) = self.per_use_root(object, false) else {
            return false;
        };
        self.cx.err(
            format!("cannot {what} module-level constant `{name}`"),
            span,
        );
        true
    }

    /// The module constant evaluated at each use that `e` gives (a part of), when it is
    /// reached through a form that hides it from the place check (`through`).
    fn per_use_root(&mut self, e: &ast::Expr, through: bool) -> Option<(String, Span)> {
        match &strip_parens(e).kind {
            ast::ExprKind::NonNull(x) | ast::ExprKind::Cast { expr: x, .. } => {
                self.per_use_root(x, true)
            }
            ast::ExprKind::Member { object, .. } | ast::ExprKind::Index { object, .. } => {
                self.per_use_root(object, through)
            }
            ast::ExprKind::Binary { op, lhs, rhs }
                if matches!(
                    op,
                    ast::BinaryOp::Nullish | ast::BinaryOp::Or | ast::BinaryOp::And
                ) =>
            {
                let l = match op {
                    ast::BinaryOp::And => None,
                    _ => self.per_use_root(lhs, true),
                };
                l.or_else(|| self.per_use_root(rhs, true))
            }
            ast::ExprKind::Cond { then, els, .. } => self
                .per_use_root(then, true)
                .or_else(|| self.per_use_root(els, true)),
            ast::ExprKind::Ident(id) if through && !self.names_local(&id.name) => {
                let Some(crate::ctx::Item::Def(d)) = self.cx.lookup_item(self.module, &id.name)
                else {
                    return None;
                };
                if !matches!(self.cx.info[d.0 as usize], crate::defs::DefInfo::Global(_)) {
                    return None;
                }
                crate::body::driver::ensure_global(self.cx, d);
                let (stored, ty, name) = self
                    .cx
                    .global(d)
                    .map(|g| (g.stored, g.ty, g.name.clone()))?;
                (!stored && !self.cx.is_copy(ty)).then_some((name, e.span))
            }
            _ => None,
        }
    }

    /// Is `ty` the type of this module's `import.meta` (`ast::IMPORT_META`)?
    fn is_import_meta_type(&mut self, ty: TyId) -> bool {
        let Some(crate::ctx::Item::Def(d)) = self.cx.lookup_item(self.module, ast::IMPORT_META)
        else {
            return false;
        };
        if !matches!(self.cx.info[d.0 as usize], crate::defs::DefInfo::Global(_)) {
            return false;
        }
        crate::body::driver::ensure_global(self.cx, d);
        self.cx.global(d).is_some_and(|g| g.ty == ty)
    }

    /// The target `v` read again after assignment `a` (`assign_value`): after `x ??= d` with a
    /// `d` that is not null, `x` is not either, also a module-level variable, which is not
    /// narrowed (`(xs ??= []).push(f)`).
    pub(super) fn nullish_assigned_read(
        &mut self,
        op: Option<ast::BinaryOp>,
        a: &hir::Expr,
        mut v: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if op != Some(ast::BinaryOp::Nullish) || !assigns_some(a) || self.is_three_state(v.ty) {
            return v;
        }
        let Some(p) = self.cx.ty.opt_payload(v.ty) else {
            return v;
        };
        set_place_mode(&mut v, UseMode::Borrow);
        let mode = self.use_mode(p, Want::Borrow);
        self.mk(H::UnwrapSome(Box::new(v), mode), p, span)
    }
}

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
        if !all && has_path_index(&inner) && self.peek_held(&inner, cast) {
            // The check after a right-hand side that may change memory reads the place again:
            // an index that is a variable or a path is bound too, so the check reads the slot
            // written (`ws[k]![0] = (() => { k = 1; return 5; })()`).
            inner = self.bind_effects(&inner, true, &mut lets);
        }
        let object = rebuild(&inner);
        self.call_temps.append(&mut lets);
        let (temps, rechecks) = (self.call_temps.len(), self.held_recheck.len());
        let mark = self.cx.diags.len();
        let p = self.held_place(&inner);
        let held = self.held_tuple(&p, cast) || self.held_member(&p, cast).is_some();
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
                self.held_recheck.push(HeldRecheck::Present(p.clone()));
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
            self.held_recheck.push(HeldRecheck::Assert(object.clone()));
            let h = H::UnwrapVariant {
                expr: Box::new(p),
                variant,
                mode: hir::UseMode::Borrow,
            };
            return self.mk(h, member, object.span);
        }
        // Not a place that holds a tuple: the checks an inner held target pushed are dropped.
        self.call_temps.truncate(temps);
        self.held_recheck.truncate(rechecks);
        self.cx.diags.truncate(mark);
        self.expr(object, None, Want::Borrow)
    }

    /// Is the checked `inner` a place that holds a tuple (`assign_receiver`)? Checked once to
    /// learn this: its errors and hidden statements are dropped.
    fn peek_held(&mut self, inner: &ast::Expr, cast: Option<&ast::TypeExpr>) -> bool {
        let (temps, rechecks) = (self.call_temps.len(), self.held_recheck.len());
        let mark = self.cx.diags.len();
        let p = self.held_place(inner);
        let held = self.held_tuple(&p, cast) || self.held_member(&p, cast).is_some();
        self.call_temps.truncate(temps);
        self.held_recheck.truncate(rechecks);
        self.cx.diags.truncate(mark);
        held
    }

    /// `f` checking an assignment: the target's hidden statements (`assign_receiver`) are
    /// returned to run before it, and a tuple place to check again stays within it.
    pub(super) fn target_scope<R>(
        &mut self,
        f: impl FnOnce(&mut Self) -> R,
    ) -> (R, Vec<hir::Stmt>) {
        let outer = std::mem::take(&mut self.call_temps);
        let recheck = std::mem::take(&mut self.held_recheck);
        let r = f(self);
        self.held_recheck = recheck;
        (r, std::mem::replace(&mut self.call_temps, outer))
    }

    /// The value `v` written into a tuple behind `p!` / `p as T` (`assign_receiver`), then a
    /// check that `v` left `p` present: a right-hand side that sets `p` to null
    /// (`ht.t![0] = (() => { ht.t = null; return 5; })()`) stops the program with a message
    /// naming that, where Node writes into the tuple `p` held before (#1352).
    pub(super) fn held_rechecked(&mut self, v: hir::Expr) -> hir::Expr {
        let rechecks = std::mem::take(&mut self.held_recheck);
        if rechecks.is_empty() || !crate::effects::may_change_memory(&v) {
            return v;
        }
        let (ty, span) = (v.ty, v.span);
        let tmp = self.new_local("<value>", ty, false, span, LocalKind::Temp);
        let mut stmts = vec![hir::Stmt {
            kind: hir::StmtKind::Let {
                local: tmp,
                init: Some(v),
            },
            span,
        }];
        // The right-hand side, or another task during its `await`, cleared the place; an
        // outer tuple (`g[0]!` of `g[0]![0]![1]`) is checked before the one inside it.
        let msg =
            "the tuple this assignment writes into was set to null while its right-hand side ran";
        for recheck in rechecks {
            let check = match recheck {
                HeldRecheck::Present(p) => self.checked_present(p, msg, None, true, span),
                HeldRecheck::Assert(object) => self.expr(&object, None, Want::Borrow),
            };
            stmts.push(hir::Stmt {
                kind: hir::StmtKind::Expr(check),
                span,
            });
        }
        let value = self.mk(H::Local(tmp, hir::UseMode::Move), ty, span);
        self.with_temps_value(stmts, value)
    }

    /// `p` (the checked `inner` of a held target) as the place that holds the tuple: a variable
    /// a closure assigns is read through a check of its narrowing, and its place is the variable
    /// itself.
    fn held_place(&mut self, inner: &ast::Expr) -> hir::Expr {
        if let Some(p) = self.nested_held_place(inner) {
            return p;
        }
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

    /// `inner` an element or member of a held target itself (`g[0]![0]` of `g[0]![0]![1]`,
    /// `hh[n]!.t` of `hh[n]!.t![0]`): that part of the outer tuple's or object's place, after
    /// its check (`assign_receiver`), so a write reaches the tuple inside it. `None`: `inner`
    /// is not such a part, or the outer target is not a held place.
    fn nested_held_place(&mut self, inner: &ast::Expr) -> Option<hir::Expr> {
        let (object, index, prop) = match &strip_parens(inner).kind {
            ast::ExprKind::Index {
                object,
                index,
                optional: false,
            } => (object, Some(index), None),
            ast::ExprKind::Member {
                object,
                prop,
                optional: false,
            } => (object, None, Some(prop)),
            _ => return None,
        };
        if !matches!(
            strip_parens(object).kind,
            ast::ExprKind::NonNull(_) | ast::ExprKind::Cast { .. }
        ) {
            return None;
        }
        let (temps, rechecks) = (self.call_temps.len(), self.held_recheck.len());
        let mark = self.cx.diags.len();
        let obj = self.assign_receiver(object);
        let part = match (&obj.kind, index, prop) {
            (H::UnwrapSome(..) | H::UnwrapVariant { .. }, Some(index), _) => {
                Some(self.index_of(obj, index, Want::Borrow, inner.span))
            }
            (H::UnwrapSome(..) | H::UnwrapVariant { .. }, _, Some(prop)) => {
                self.field_access(obj, prop, Want::Borrow, inner.span)
            }
            _ => None,
        };
        if part.is_none() {
            self.call_temps.truncate(temps);
            self.held_recheck.truncate(rechecks);
            self.cx.diags.truncate(mark);
        }
        part
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
pub(super) fn is_hoisted(e: &ast::Expr) -> bool {
    matches!(&strip_parens(e).kind, ast::ExprKind::Ident(id) if id.name.starts_with('<'))
}

pub(super) fn is_literal(e: &ast::Expr) -> bool {
    matches!(&strip_parens(e).kind, ast::ExprKind::Lit(_))
        || super::member_keys::literal_key(e).is_some()
}

/// Are the indexes of the held target `e` free of effects?
pub(super) fn pure_indexes(e: &ast::Expr) -> bool {
    match &strip_parens(e).kind {
        ast::ExprKind::Member { object, .. }
        | ast::ExprKind::NonNull(object)
        | ast::ExprKind::Cast { expr: object, .. } => pure_indexes(object),
        ast::ExprKind::Index { object, index, .. } => is_pure_value(index) && pure_indexes(object),
        _ => true,
    }
}

/// Has the held target `e` an index that is not a literal or a bound temporary?
fn has_path_index(e: &ast::Expr) -> bool {
    match &strip_parens(e).kind {
        ast::ExprKind::Member { object, .. }
        | ast::ExprKind::NonNull(object)
        | ast::ExprKind::Cast { expr: object, .. } => has_path_index(object),
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

/// Does assignment `h` store a value that is not a nothing (`x = x ?? d` where `d` is not null:
/// a `Some(…)`, in each arm of a conditional)?
fn assigns_some(h: &hir::Expr) -> bool {
    fn some(v: &hir::Expr) -> bool {
        match &v.kind {
            H::WrapSome(_) => true,
            H::If { then, els, .. } => some(then) && some(els),
            H::Block(b) => b.value.as_deref().is_some_and(some),
            _ => false,
        }
    }
    match &h.kind {
        H::Assign { value, .. } => some(value),
        H::Block(b) => {
            b.value.as_deref().is_some_and(assigns_some)
                || b.stmts
                    .last()
                    .is_some_and(|s| matches!(&s.kind, hir::StmtKind::Expr(e) if assigns_some(e)))
        }
        _ => false,
    }
}
