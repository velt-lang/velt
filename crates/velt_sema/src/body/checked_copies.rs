//! Where a narrowed value that a call may have changed reads as its payload, and untyped copies
//! of one (#1280).
//!
//! A module variable (`global_narrow`) or a field path (`field_narrow`, also `r[k]`) that a
//! null check narrowed may be set to `null` by any call before a read, which TypeScript ignores.
//! Such a read is the payload, checked where it happens, only where the payload is needed: as a
//! receiver (`x.m()`, `x.f = v`), a callee, an operand of arithmetic or a comparison
//! (`n + 1`, `n > 2`), a spread (`...xs`), a value of a non-nullable type (`use(x)` with
//! `a: Foo`, `return x`), or what a write goes through. Everywhere else (`console.log(x)`, a
//! template, `x === y`, an untyped object or array element) the value is read as it is, so a
//! program that reads it after a call cleared it sees `null`, as in JavaScript.
//!
//! An untyped `let c = x` / `const c = x` of such a value is a *checked copy*: `c` has the
//! nullable type and the narrowing of `x`, and its reads follow the same rule, checked where the
//! payload is needed (`c.n` stops with a `TypeError` where JavaScript throws one, `console.log(c)`
//! prints `null`). A closure that captures it checks its reads too.

use velt_common::Span;
use velt_syntax::ast;

use super::narrow::Fact;
use super::{FnCx, Want};
use crate::hir::{self, ExprKind as H, LocalId, TyId};

impl FnCx<'_, '_> {
    /// Does the read at `span` (expected type `exp`) need the payload of a narrowed value of
    /// type `raw` (where a `raw` is expected, the value as it is will do)?
    pub(crate) fn needs_payload(
        &self,
        span: Span,
        exp: Option<TyId>,
        want: Want,
        taken: bool,
        raw: TyId,
    ) -> bool {
        if self.raw_read == Some(span) {
            return false;
        }
        if want == Want::BorrowMut || self.payload_spans.contains(&span) {
            return true;
        }
        // With `taken` (a field path of a local, which was always its payload before), a value
        // taken where no type is expected (an untyped array or object element) is the payload,
        // as TypeScript types it; an untyped `let c = x` is a checked copy instead.
        if taken && want == Want::Move && exp.is_none() {
            return true;
        }
        exp.is_some_and(|t| {
            t != raw
                && self.cx.dynamic(t).is_none()
                && !matches!(self.cx.ty.kind(t), hir::TyKind::Param(_))
        })
    }

    /// `f` with `e`'s value taken as a payload (`needs_payload`).
    pub(crate) fn with_payload<R>(&mut self, e: &ast::Expr, f: impl FnOnce(&mut Self) -> R) -> R {
        let mut inner = e;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        self.with_payload_at(inner.span, f)
    }

    /// `f` with the read at `span` taken as a payload (a callee name).
    pub(crate) fn with_payload_at<R>(&mut self, span: Span, f: impl FnOnce(&mut Self) -> R) -> R {
        self.payload_spans.push(span);
        let r = f(self);
        self.payload_spans.pop();
        r
    }

    /// `e` checked as an operand whose payload is used (`n + 1`, `...xs`).
    pub(crate) fn payload_expr(
        &mut self,
        e: &ast::Expr,
        exp: Option<TyId>,
        want: Want,
    ) -> hir::Expr {
        self.with_payload(e, |s| s.expr(e, exp, want))
    }

    /// `object` evaluated as the receiver of a member access or call.
    pub(crate) fn receiver_expr(&mut self, object: &ast::Expr) -> hir::Expr {
        self.payload_expr(object, None, Want::Borrow)
    }

    /// The initializer `e` of an untyped `let` or `const`: a narrowed module variable or field
    /// path is read as it is (the local is a checked copy, `note_checked_copy`).
    pub(crate) fn copy_init(&mut self, e: &ast::Expr, want: Want) -> hir::Expr {
        let mut inner = e;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        let path = matches!(
            inner.kind,
            ast::ExprKind::Ident(_)
                | ast::ExprKind::Member {
                    optional: false,
                    ..
                }
                | ast::ExprKind::Index {
                    optional: false,
                    ..
                }
        );
        if !path || self.narrowed_source(inner).is_none() {
            return self.expr(e, None, want);
        }
        // A function stays its payload (`const f = o.cb; f()`), as before.
        let callable = self
            .peek_ty(inner)
            .and_then(|t| self.cx.ty.opt_payload(t))
            .is_some_and(|p| {
                matches!(
                    self.cx.ty.kind(p),
                    hir::TyKind::Closure(_) | hir::TyKind::FnPtr { .. }
                )
            });
        if callable {
            return self.payload_expr(e, None, want);
        }
        let outer = self.raw_read.replace(inner.span);
        let h = self.expr(e, None, want);
        self.raw_read = outer;
        h
    }

    /// The untyped local `local`, just declared from `init`: a checked copy when `init` is a
    /// narrowed module variable or field path read as it is.
    pub(crate) fn note_checked_copy(
        &mut self,
        local: LocalId,
        untyped: bool,
        init: Option<&ast::Expr>,
    ) {
        let Some(init) = init.filter(|_| untyped) else {
            return;
        };
        let mut inner = init;
        while let ast::ExprKind::Paren(x) = &inner.kind {
            inner = x;
        }
        if self.cx.ty.opt_payload(self.local_ty(local)).is_none() {
            return;
        }
        let token = self.narrowed_source(inner);
        if token.is_some() {
            self.f.checked_copies.insert(local);
            self.narrow(&Fact::NonNull(local));
        }
    }

    /// The token of `inner` (a name or a field path) when a null test narrowed it and a call
    /// may have changed it since: a module variable, a variable a closure assigns, a field path.
    fn narrowed_source(&mut self, inner: &ast::Expr) -> Option<LocalId> {
        let token = match &inner.kind {
            // A variable a closure assigns is checked the same way (`closure_assigned`).
            ast::ExprKind::Ident(_) => match self.named_local(inner) {
                Some(l) if self.closure_assignment(l).is_some() => Some(l),
                Some(_) => None,
                None => self.existing_global_token(inner),
            },
            ast::ExprKind::Member {
                optional: false, ..
            }
            | ast::ExprKind::Index {
                optional: false, ..
            } => self.existing_field_token(inner),
            _ => None,
        };
        token.filter(|t| self.is_narrowed(*t))
    }

    /// Is local `l` (of this frame, or the variable it captures) a checked copy?
    pub(crate) fn is_checked_copy(&self, l: LocalId) -> bool {
        let mut l = l;
        for f in std::iter::once(&self.f).chain(self.outer.iter().rev()) {
            if f.checked_copies.contains(&l) {
                return true;
            }
            match f.captures.iter().find(|c| c.inner == l) {
                Some(c) => l = c.outer,
                None => return false,
            }
        }
        false
    }

    /// A read of checked copy `l` that does not need its payload: the value as it is.
    pub(crate) fn checked_copy_read(
        &mut self,
        l: LocalId,
        exp: Option<TyId>,
        want: Want,
        span: Span,
    ) -> Option<hir::Expr> {
        if !self.is_checked_copy(l)
            || !self.is_narrowed(l)
            || self.needs_payload(span, exp, want, false, self.local_ty(l))
        {
            return None;
        }
        let ty = self.local_ty(l);
        let mode = self.use_mode(ty, want);
        Some(self.mk(H::Local(l, mode), ty, span))
    }
}
