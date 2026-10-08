//! Name expressions: locals (narrowed `T | null` locals read as `UnwrapSome`, union locals
//! narrowed to one member as `UnwrapVariant`, locals narrowed to a subclass by `instanceof` as
//! `Downcast`), `this`, module
//! constants (`Global`) and named functions used as values (`FnRef`).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::ctx::Item;
use crate::defs::{DefInfo, FnKind};
use crate::hir::{self, DefId, ExprKind as H, LocalId, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    pub(crate) fn local_expr(&mut self, l: LocalId, want: Want, span: Span) -> hir::Expr {
        self.note_refused_read(l, span);
        let ty = self.local_ty(l);
        if want != Want::BorrowMut && self.narrowed_to_nothing(l) {
            // No member is left (e.g. in the `default` of an exhaustive `switch`): the read
            // has type `never`, as in TS (`const _x: never = s;`).
            let msg = self.str_lit("unreachable: a value narrowed to `never` was read", span);
            let never = self.cx.ty.never;
            return self.intrinsic(hir::Intrinsic::Panic, vec![msg], never, span);
        }
        let payload = self.cx.ty.opt_payload(ty).filter(|_| self.is_narrowed(l));
        let member = self.narrowed_member(l, payload.unwrap_or(ty), payload.is_some());
        if member.is_none() {
            if let Some(e) = self.union_narrowed_to_class(l, ty, payload, want, span) {
                return e;
            }
        }
        if payload.is_none() && member.is_none() {
            let mode = self.use_mode(ty, want);
            let e = self.mk(H::Local(l, mode), ty, span);
            return self.downcast_narrowed(l, e);
        }
        let base_mode = if want == Want::BorrowMut {
            UseMode::BorrowMut
        } else {
            UseMode::Borrow
        };
        let mut e = self.mk(H::Local(l, base_mode), ty, span);
        if let Some(p) = payload {
            let mode = match member {
                Some(_) => base_mode,
                None => self.use_mode(p, want),
            };
            e = self.mk(H::UnwrapSome(Box::new(e), mode), p, span);
        }
        if let Some((variant, m)) = member {
            let mode = self.use_mode(m, want);
            let kind = H::UnwrapVariant {
                expr: Box::new(e),
                variant,
                mode,
            };
            e = self.mk(kind, m, span);
        }
        self.downcast_narrowed(l, e)
    }

    /// A union local `l` (of type `ty`, `T | null` narrowed to `payload`) that `instanceof`
    /// narrowed to a class every member it can still hold is, or is a base of: read as that
    /// class (`catch (e)` on `Error | MyErr`, then `e instanceof MyErr`).
    fn union_narrowed_to_class(
        &mut self,
        l: LocalId,
        ty: TyId,
        payload: Option<TyId>,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        let target = self.narrowed_class(l)?;
        let u = payload.unwrap_or(ty);
        self.cx.union_def(u)?;
        if payload.is_none() && self.cx.ty.opt_payload(ty).is_some() {
            return None;
        }
        let mode = self.use_mode(ty, want);
        let mut e = self.mk(H::Local(l, mode), ty, span);
        if let Some(p) = payload {
            let base = self.mk(H::Local(l, UseMode::Borrow), ty, span);
            let inner = self.use_mode(p, want);
            e = self.mk(H::UnwrapSome(Box::new(base), inner), p, span);
        }
        self.union_downcast(e, target).ok()
    }

    /// The read `e` of local `l` as the subclass `l` is narrowed to by `instanceof`, when `e`
    /// is of a base class of it or an interface type (not, say, a union of several members).
    pub(crate) fn downcast_narrowed(&mut self, l: LocalId, e: hir::Expr) -> hir::Expr {
        let Some(target) = self.narrowed_class(l) else {
            return e;
        };
        if !self.downcast_applies(e.ty, target) {
            return e;
        }
        let span = e.span;
        self.mk(H::Downcast(Box::new(e)), target, span)
    }

    /// Does a value of type `from` read as the subclass `to` it is narrowed to (`from` is a
    /// base class of `to`, or an interface type)?
    pub(crate) fn downcast_applies(&self, from: TyId, to: TyId) -> bool {
        match (self.cx.class_of(from), self.cx.class_of(to)) {
            (Some((f, _)), Some((t, _))) => f != t && self.cx.class_extends(t, f),
            _ => matches!(self.cx.ty.kind(from), TyKind::Dyn(..)),
        }
    }

    /// Has flow narrowing ruled out every member of union local `l` (and `null`)?
    fn narrowed_to_nothing(&self, l: LocalId) -> bool {
        let nullable = self.cx.ty.opt_payload(self.local_ty(l)).is_some();
        self.allowed_members(l).is_some_and(|vs| vs.is_empty())
            && (!nullable || self.is_narrowed(l))
    }

    /// The one member (variant, type) union local `l` is narrowed to, if any (`u`: the union,
    /// the payload of `l`'s type when `unwrapped`).
    fn narrowed_member(&mut self, l: LocalId, u: TyId, unwrapped: bool) -> Option<(u32, TyId)> {
        let nullable = self.cx.ty.opt_payload(self.local_ty(l)).is_some();
        if nullable && !unwrapped {
            return None;
        }
        let members = self.cx.union_members(u)?;
        match self.allowed_members(l)?.as_slice() {
            [v] => Some((*v, members[*v as usize])),
            _ => None,
        }
    }

    pub(crate) fn this_expr(&mut self, want: Want, span: Span) -> hir::Expr {
        if self.this_before_super() {
            self.cx.err(
                "'super' must be called before accessing 'this' in the constructor of a derived class",
                span,
            );
            return self.error_expr(span);
        }
        match self.lookup_local("this", span) {
            Some(l) => {
                self.rec_local(span, l);
                self.local_expr(l, want, span)
            }
            None if self.generic_arrow => {
                self.cx.error(
                    Diagnostic::error("`this` cannot be used in a generic arrow function", span)
                        .with_note("a local generic arrow function is a generic function nested in this one: it cannot use `this` or the local variables of enclosing functions")
                        .with_note("pass the value it needs as a parameter, or drop the type parameters to make it a closure"),
                );
                self.error_expr(span)
            }
            None => {
                self.cx.err(
                    "`this` is only available inside methods and constructors",
                    span,
                );
                self.error_expr(span)
            }
        }
    }

    pub(crate) fn ident_expr(
        &mut self,
        id: &ast::Ident,
        exp: Option<TyId>,
        want: Want,
    ) -> hir::Expr {
        if let Some(l) = self.lookup_local(&id.name, id.span) {
            self.rec_local(id.span, l);
            return self.local_expr(l, want, id.span);
        }
        match self.lookup_item(&id.name, id.span) {
            Some(Item::Def(d)) => return self.def_value(d, id, exp, want),
            Some(Item::Alias(_)) => {
                self.cx
                    .err(format!("`{}` is a type, not a value", id.name), id.span);
                return self.error_expr(id.span);
            }
            None => {}
        }
        if self.is_namespace(&id.name) {
            self.cx.err(
                format!(
                    "`{}` is a namespace import: use its exported members, like `{}.name`",
                    id.name, id.name
                ),
                id.span,
            );
            return self.error_expr(id.span);
        }
        match id.name.as_str() {
            "console" | "process" | "panic" | "Math" | "spawn" | "sleep" | "yieldNow"
            | "Promise" | "performance" | "Date" => {
                self.cx.err(
                    format!(
                        "`{}` can only be used in a call like `console.log(...)`",
                        id.name
                    ),
                    id.span,
                );
            }
            _ => self.unknown_name(id),
        }
        self.error_expr(id.span)
    }

    /// "cannot find `x`", or — inside a nested declaration — that `x` is a local of an
    /// enclosing function (nested functions do not capture).
    pub(crate) fn unknown_name(&mut self, id: &ast::Ident) {
        if self.unknown_namespace_member(&id.name, id.span) {
            return;
        }
        if self.fn_expr_self_ref(&id.name, id.span) {
            return;
        }
        if !self.enclosing_locals.contains(&id.name) {
            self.cx
                .err(format!("cannot find `{}` in this scope", id.name), id.span);
            return;
        }
        if self.generic_arrow {
            self.cx.error(
                Diagnostic::error(
                    format!("`{}` cannot be captured by a generic arrow function", id.name),
                    id.span,
                )
                .with_note("a local generic arrow function is a generic function nested in this one: it cannot use the local variables of enclosing functions")
                .with_note(format!("pass `{}` as a parameter, or drop the type parameters to make it a closure", id.name)),
            );
            return;
        }
        self.cx.error(
            Diagnostic::error(
                format!("`{}` cannot be captured by a nested function", id.name),
                id.span,
            )
            .with_note("nested declarations cannot use the local variables of enclosing functions")
            .with_note("use an arrow function instead: `const f = (...) => { ... };`"),
        );
    }

    fn def_value(&mut self, d: DefId, id: &ast::Ident, exp: Option<TyId>, want: Want) -> hir::Expr {
        let span = id.span;
        match &self.cx.info[d.0 as usize] {
            DefInfo::Fn(_) => self.fn_ref(d, id, exp),
            DefInfo::Global(_) => self.global_read(d, want, span),
            _ => {
                self.cx
                    .err(format!("`{}` is a type, not a value", id.name), span);
                self.error_expr(span)
            }
        }
    }

    pub(crate) fn global_read(&mut self, d: DefId, want: Want, span: Span) -> hir::Expr {
        crate::body::driver::ensure_global(self.cx, d);
        let g = self.cx.global(d).expect("ICE: global");
        let (ty, name) = (g.ty, g.name.clone());
        let fresh = g
            .init
            .as_ref()
            .is_some_and(crate::body::pure_init::has_call);
        if want == Want::Move && fresh && !self.cx.is_copy(ty) {
            // A call computes a new value at each use: another reference to it is owned.
            let read = self.mk(H::Global(d), ty, span);
            return self.intrinsic(hir::Intrinsic::Share, vec![read], ty, span);
        }
        if want == Want::Move && !self.cx.is_copy(ty) && ty != self.cx.ty.str_ {
            self.cx.error(
                Diagnostic::error(format!("cannot move out of module constant `{name}`"), span)
                    .with_note(format!("use `{name}.clone()` for an owned copy")),
            );
        }
        self.mk(H::Global(d), ty, span)
    }

    /// A named function used as a value; generic ones take their type args from `exp`.
    fn fn_ref(&mut self, d: DefId, id: &ast::Ident, exp: Option<TyId>) -> hir::Expr {
        let span = id.span;
        let f = self.cx.fn_info(d);
        if f.kind == FnKind::Extern {
            self.cx.err(
                format!("external function `{}` cannot be used as a value", id.name),
                span,
            );
            return self.error_expr(span);
        }
        let (n, params, is_async) = (
            f.generics.len(),
            f.params.iter().map(|p| p.ty).collect::<Vec<_>>(),
            f.is_async,
        );
        let ret = crate::body::returns::ret_of(self.cx, d, span);
        let fn_ty = self.fn_value_type(d, params, ret, is_async);
        let mut slots = vec![None; n];
        if let Some(e) = self.hint(exp) {
            self.cx.match_ty(fn_ty, e, &mut slots);
        }
        let args: Vec<TyId> = slots
            .iter()
            .map(|s| s.unwrap_or(self.cx.ty.error))
            .collect();
        if slots.iter().any(Option::is_none) {
            self.cx.error(
                Diagnostic::error(
                    format!("cannot infer the type arguments of generic function `{}`", id.name),
                    span,
                )
                .with_note("use it where a function type is expected, e.g. `const f: (x: i64) => i64 = ...`"),
            );
        }
        let ty = self.cx.subst(fn_ty, &args);
        let ty = self.fn_value_errors(d, &args, ty, exp, span);
        self.cx.fn_values.push((d, args.clone(), span));
        self.mk(H::FnRef(d, args), ty, span)
    }

    /// The function type of named function `d` used as a value, with what it is known to throw
    /// (an async function's promise rejects with it).
    fn fn_value_type(&mut self, d: DefId, params: Vec<TyId>, ret: TyId, is_async: bool) -> TyId {
        if is_async {
            let ret = self.async_call_ret(d, ret);
            return self.cx.ty.fn_ptr(params, ret);
        }
        let never = self.cx.ty.never;
        if self.is_generator_fn(d) {
            // Calling a generator only creates it: its errors are the result's `E`.
            let e = crate::throws::throws_now(self.cx, d, &[]).unwrap_or(never);
            let ret = self.cx.with_generator_error(ret, e);
            return self.cx.ty.fn_ptr(params, ret);
        }
        let throws = crate::throws::throws_now(self.cx, d, &[]).unwrap_or(never);
        self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
        })
    }

    /// A synchronous function value takes the error type the context expects when it throws
    /// less (lowering's function-value thunk converts its errors); the final error type of `d`
    /// is checked against the one used here.
    fn fn_value_errors(
        &mut self,
        d: DefId,
        args: &[TyId],
        ty: TyId,
        exp: Option<TyId>,
        span: Span,
    ) -> TyId {
        let TyKind::FnPtr {
            params,
            ret,
            throws,
        } = self.cx.ty.kind(ty).clone()
        else {
            return ty;
        };
        let wanted = match self.hint(exp).map(|e| self.cx.ty.kind(e).clone()) {
            Some(TyKind::FnPtr { throws: t, .. }) if !self.cx.ty.has_error(t) => Some(t),
            _ => None,
        };
        let never = self.cx.ty.never;
        let fits = |s: &mut Self, w: TyId| {
            let (w, t) = (s.cx.canon_error(Some(w)), s.cx.canon_error(Some(throws)));
            s.cx.error_outside(w, t).is_none()
        };
        let is_generator = self.is_generator_fn(d);
        let is_async = self.cx.fn_info(d).is_async || is_generator;
        let throws = match wanted {
            Some(w) if !is_async && fits(self, w) => w,
            _ => throws,
        };
        let observed = if is_generator {
            self.cx
                .generator_result(ret)
                .and_then(|(_, a)| a.get(1).copied())
        } else if is_async {
            self.cx.ty.promise_error(ret)
        } else {
            Some(throws)
        };
        self.cx.throw_checks.push(crate::throws::ThrowCheck {
            srcs: vec![crate::defs::ThrowSrc::Call(d, args.to_vec(), span)],
            observed: observed.filter(|t| *t != never),
            exact: is_async,
            span,
        });
        self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
        })
    }
}
