//! Optional chains, `a?.b`, `a?.m()`, `a?.[i]`, `f?.()`, desugared into matches on `T | null`
//! (`matching.rs` has the other null-handling operators).

use velt_common::Span;
use velt_syntax::ast;

use crate::body::places::{place_root, set_place_mode};
use crate::body::{FnCx, Want};
use crate::hir::{self, ExprKind as H, Nothing, PatKind as P, TyId, UseMode};

impl FnCx<'_, '_> {
    /// `object?.<rest>`: apply `f` to the present payload; `null` or `undefined` short-circuits
    /// to `undefined`.
    pub(crate) fn optional_chain(
        &mut self,
        object: &ast::Expr,
        span: Span,
        f: impl FnOnce(&mut Self, hir::Expr) -> hir::Expr,
    ) -> hir::Expr {
        let nothing = std::mem::take(&mut self.chain_nothing);
        if let Some((inner, prop)) = self.nothing_link_parts(object, nothing) {
            return self.nothing_link(inner, prop, span, f);
        }
        let s = self.link_object(object, nothing, |s| {
            s.test_operand(object, None, Want::Borrow)
        });
        self.optional_chain_rest(s, Some(object), nothing, span, f)
    }

    /// `optional_chain` once its object is checked (`s`; `object` its syntax, when it has one).
    fn optional_chain_rest(
        &mut self,
        s: hir::Expr,
        object: Option<&ast::Expr>,
        nothing: bool,
        span: Span,
        f: impl FnOnce(&mut Self, hir::Expr) -> hir::Expr,
    ) -> hir::Expr {
        let s = self.flatten_chain(s);
        // `any`, and `object` / `Function`, which hold `null` / `undefined` too (issue #926).
        if self
            .cx
            .dynamic(s.ty)
            .is_some_and(|d| d != crate::any::Dynamic::Unknown)
        {
            let (l, mode) = self.option_binding(&s, s.ty, "<chain>", false);
            let text = self.callee_text(&s);
            self.f.chain_texts.push((l, text));
            let use_mode = match mode {
                UseMode::Move => UseMode::Borrow,
                m => m,
            };
            let v = self.mk(H::Local(l, use_mode), s.ty, span);
            let r = f(self, v);
            return self.dynamic_chain_match(s, l, mode, r, span);
        }
        // A `T | null` narrowed to its `T`, which may hold a view of a nothing (`null!`): the
        // option itself, whose pattern sees that nothing (velt_vir `option_present`).
        let s = match s.kind {
            H::UnwrapSome(base, _) if nothing && self.may_hold_nothing(s.ty) => *base,
            _ => s,
        };
        let Some(payload) = self.present_payload(s.ty) else {
            if nothing && self.may_hold_nothing(s.ty) {
                return self.nothing_chain(s, span, f);
            }
            return f(self, s);
        };
        let (l, mode) = self.option_binding(&s, payload, "<chain>", false);
        if let Some(object) = object {
            self.note_readonly_binding(l, object, &s);
        }
        // A moved-in payload (the object was a temporary) is owned by the binding, which drops
        // it after the rest: the rest uses it as a place, moving out of it only if it consumes it.
        let use_mode = match mode {
            UseMode::Move => UseMode::Borrow,
            m => m,
        };
        let v = self.mk(H::Local(l, use_mode), payload, span);
        let mut r = f(self, v);
        // A binding that borrows the object (a place) does not own the payload: the rest reads
        // through it without moving out of it, so a consumer shares what it reads
        // (`const s = rec?.state` moved the string out of `rec`, which released it again).
        if mode == UseMode::Borrow && place_root(&r) == Some(l) && moves_out(&r) {
            set_place_mode(&mut r, UseMode::Borrow);
        }
        self.chain_match(s, l, mode, payload, r, span)
    }

    /// The object `inner` and member `prop` of a link `object` (`f?.inner` in `f?.inner?.v`) that
    /// `nothing_link` checks: a chain that takes `undefined` on an `inner` of a type with no
    /// nothing that may hold a view of one.
    fn nothing_link_parts<'a>(
        &mut self,
        object: &'a ast::Expr,
        nothing: bool,
    ) -> Option<(&'a ast::Expr, &'a ast::Ident)> {
        if !nothing {
            return None;
        }
        let ast::ExprKind::Member {
            object: inner,
            prop,
            optional: true,
        } = &super::assign_paths::strip_parens(object).kind
        else {
            return None;
        };
        let t = self.peek_ty(inner)?;
        let fits = self.cx.dynamic(t).is_none()
            && self.present_payload(t).is_none()
            && self.may_hold_nothing(t);
        fits.then_some((inner, prop))
    }

    /// `inner?.prop?.<rest>` taking `undefined`, where `inner` may be a view of a nothing:
    /// `undefined` when `inner` is one, else `inner.prop?.<rest>` on the field itself, read in
    /// place. The chain tests `inner` once, and the field's own option once.
    fn nothing_link(
        &mut self,
        inner: &ast::Expr,
        prop: &ast::Ident,
        span: Span,
        f: impl FnOnce(&mut Self, hir::Expr) -> hir::Expr,
    ) -> hir::Expr {
        let mut s = self.link_object(inner, true, |s| s.test_operand(inner, None, Want::Borrow));
        let ty = s.ty;
        let (l, mode) = self.option_binding(&s, ty, "<chain>", false);
        if mode == UseMode::Move {
            self.force_move(&mut s);
        }
        let read = match mode {
            UseMode::Move => UseMode::Borrow,
            m => m,
        };
        let v = self.mk(H::Local(l, read), ty, span);
        let field = self.member_of(v, prop, Want::Borrow, span);
        let r = self.optional_chain_rest(field, None, true, span, f);
        self.nothing_chain_match(s, (l, mode), r, span)
    }

    /// `match (s) { l => r (wrapped as `T | undefined`), nothing => undefined }`: the short-circuit of
    /// an optional chain whose rest `r` was checked on the payload bound to `l`.
    pub(super) fn chain_match(
        &mut self,
        s: hir::Expr,
        l: hir::LocalId,
        mode: UseMode,
        payload: TyId,
        r: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if self.cx.ty.is_bottom(r.ty) {
            return r;
        }
        // The short-circuit gives `undefined`, on a `T | null` object too, as in JavaScript
        // (issue #930).
        let nothing = Nothing::Undefined;
        let (ty, body) = if nothing == Nothing::Undefined
            && self.cx.ty.nothing_of(r.ty) == Some(Nothing::Null)
        {
            // A `T | null` rest: the chain gives `T | null | undefined`.
            let t = self.cx.ty.option_of(r.ty, Nothing::Undefined);
            (t, self.mk(H::WrapSome(Box::new(r)), t, span))
        } else if r.ty == self.cx.ty.unit || self.cx.ty.opt_payload(r.ty).is_some() {
            (r.ty, r)
        } else {
            let t = self.cx.ty.option_of(r.ty, nothing);
            (t, self.mk(H::WrapSome(Box::new(r)), t, span))
        };
        let none_body = if ty == self.cx.ty.unit {
            self.unit_expr(span)
        } else {
            self.mk(H::Lit(hir::Lit::Null), ty, span)
        };
        let sty = s.ty;
        let some = self.pat(P::Binding(l, mode), payload, span);
        let (guard, rest) = self.present_guard(sty, l, payload, span);
        let arms = vec![
            hir::Arm {
                pat: self.present_pat(some, sty, span),
                guard,
                body,
            },
            hir::Arm {
                pat: rest,
                guard: None,
                body: none_body,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, ty, span)
    }
}

/// Does the place `e` move its value out (its outermost use is `Move`)?
fn moves_out(e: &hir::Expr) -> bool {
    match &e.kind {
        H::Field { mode, .. } | H::Index { mode, .. } | H::UnwrapSome(_, mode) => {
            *mode == UseMode::Move
        }
        H::UnwrapVariant { mode, .. } => *mode == UseMode::Move,
        H::Downcast(x) => moves_out(x),
        _ => false,
    }
}
