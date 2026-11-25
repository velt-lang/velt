//! The polymorphic `this` type as a method's result (`register(): this { ...; return this; }`,
//! https://veltlang.com/docs/reference/classes/ "Returning `this`"). The signature resolves `this` to the class
//! declaring the method (`resolve::TyEnv::this_ty`); a call on a receiver of a subclass gives
//! the receiver's own type, as in TypeScript, so chained calls keep the subclass's methods.
//!
//! That conversion is a `Downcast` of the result, which is sound because such a method can only
//! return `this` itself: `return this;`, or the result of another `this`-returning method called
//! on `this` (checked by `check_this_return`; anything else is TypeScript's TS2322).

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, LocalKind};
use crate::hir::{self, DefId, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// The receiver's type and whether it is `this` (`is_this_value`), for `this_result`. A
    /// value of a type parameter read as its class bound (`class_bound`, an `Upcast`) has the
    /// parameter's type.
    pub(super) fn this_recv(&self, recv: &hir::Expr) -> (TyId, bool) {
        let ty = match &recv.kind {
            H::Upcast(x) if matches!(self.cx.ty.kind(x.ty), TyKind::Param(_)) => x.ty,
            _ => recv.ty,
        };
        (ty, self.is_this_value(recv))
    }

    /// The call `call` of method `def` on a receiver of type `to` (before its conversion to the
    /// declaring class; `on_this`: the receiver is `this`): when `def` returns `this`, typed as
    /// the receiver's class.
    pub(super) fn this_result(
        &mut self,
        call: hir::Expr,
        def: DefId,
        (to, on_this): (TyId, bool),
    ) -> hir::Expr {
        if !self.cx.fn_info(def).returns_this() {
            return call;
        }
        if on_this {
            self.this_calls.insert(call.span);
        }
        if crate::hand_back::hands_back(self.cx, def) {
            return self.hand_back_call(call, def, to);
        }
        // A receiver of a type parameter bounded by the class (`x: T` with `T extends Base`)
        // gives `T`, as in TypeScript: each instance downcasts to its own class.
        if to == call.ty
            || self.cx.ty.is_bottom(call.ty)
            || !matches!(self.cx.ty.kind(to), TyKind::Adt(..) | TyKind::Param(..))
        {
            return call;
        }
        let span = call.span;
        self.mk(H::Downcast(Box::new(call)), to, span)
    }

    /// Is `h` the method's `this` (or a `this`-returning method's result on it)?
    fn is_this_value(&self, h: &hir::Expr) -> bool {
        if self.this_casts.contains(&h.span) {
            return false;
        }
        match &h.kind {
            H::Local(l, _) => {
                self.outer.is_empty()
                    && (self.local_kind(*l) == LocalKind::This || self.this_aliases.contains(l))
            }
            H::Upcast(x) | H::Downcast(x) => self.is_this_value(x),
            H::Call { .. } => self.this_calls.contains(&h.span),
            // A call that hands back its receiver (`hand_back`), the receiver being `this`.
            H::Block(b) if self.is_hand_back(h) && self.this_calls.contains(&h.span) => {
                b.value.as_deref().is_some_and(|v| self.is_this_value(v))
            }
            _ => false,
        }
    }

    /// `const local: ty = init` (`kind`, `init` checked as `h`): a `const` holding `this` is
    /// `this` (TypeScript types it `this`), so a method returning `this` may return it, and so
    /// is a `const` holding such a `const`. Not with a written type other than `this` (`const
    /// r: A = this`), which TypeScript types as the class (as it does `this as A`, `cast`).
    pub(crate) fn note_this_alias(
        &mut self,
        (local, name): (hir::LocalId, Span),
        kind: LocalKind,
        ty: Option<&ast::TypeExpr>,
        h: Option<&hir::Expr>,
    ) {
        let typed_this = ty.is_none_or(is_this_written) && !self.this_defaulted.contains(&name);
        if kind == LocalKind::Const && typed_this && h.is_some_and(|h| self.is_this_value(h)) {
            self.this_aliases.insert(local);
        }
    }

    /// The bindings of pattern `p` declared under a default (`[r = o]`, `{ a: [r] = d }`): TypeScript
    /// types each with the default's type too (`this | A` for `const [r = o] = [this]`), so none
    /// is `this`, even where the default never applies and is left out (`literal_patterns`).
    pub(crate) fn note_defaulted_bindings(&mut self, p: &ast::Pattern) {
        defaulted(p, false, &mut self.this_defaulted);
    }

    /// `h`, checked from `… as ty`: when it is `this` and `ty` is not `this`, it is typed as
    /// the class from now on, as in TypeScript (`return this as A;` is TS2322). Cast back to
    /// `this` (`this as A as this`), it is `this` again.
    pub(super) fn note_this_cast(&mut self, ty: &ast::TypeExpr, h: &hir::Expr) {
        if is_this_written(ty) {
            self.this_casts.remove(&h.span);
        } else if self.is_this_value(h) {
            self.this_casts.insert(h.span);
        }
    }

    /// `return h;` in the body: in a method returning `this`, `h` must be `this`.
    pub(crate) fn check_this_return(&mut self, h: &hir::Expr, ret: TyId, span: Span) {
        let Some(d) = self.body_def else {
            return;
        };
        if !self.outer.is_empty() || !self.cx.fn_info(d).returns_this() {
            return;
        }
        if self.is_this_value(h) || self.cx.ty.is_bottom(h.ty) {
            return;
        }
        let found = self.cx.display(h.ty);
        let class = self.cx.display(ret);
        let note = match self.cx.fn_info(d).inherits_this {
            true => format!(
                "TypeScript's TS2416: `{}` overrides a method returning `this` (the class of the object it is called on, which may be a subclass of `{class}`), so it must return `this` too",
                self.cx.fn_info(d).name.rsplit('.').next().unwrap_or_default()
            ),
            false => format!(
                "TypeScript's TS2322: `this` is the class of the object the method is called on, which may be a subclass of `{class}`; return `this`"
            ),
        };
        self.cx.error(
            Diagnostic::error(
                format!("type `{found}` is not assignable to type `this`"),
                span,
            )
            .with_note(note),
        );
    }
}

/// Into `out`, the spans of the bindings of `p` under a default (`under`: `p` is).
fn defaulted(p: &ast::Pattern, under: bool, out: &mut rustc_hash::FxHashSet<Span>) {
    use ast::PatternKind as P;
    match &p.kind {
        P::Ident(n) if under => _ = out.insert(n.span),
        P::Ident(_) | P::Wildcard => {}
        P::Object { fields, rest } => {
            fields.iter().for_each(|(_, p)| defaulted(p, under, out));
            rest.iter().filter(|_| under).for_each(|r| _ = out.insert(r.span));
        }
        P::Array { elems, rest } => {
            elems.iter().for_each(|p| defaulted(p, under, out));
            rest.iter().filter(|_| under).for_each(|r| _ = out.insert(r.span));
        }
        P::Default { pattern, .. } => defaulted(pattern, true, out),
    }
}

/// Is `t` the written type `this`?
fn is_this_written(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, args }
        if path.len() == 1 && path[0].name == "this" && args.is_empty())
}
