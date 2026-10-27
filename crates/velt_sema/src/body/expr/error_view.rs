//! Error unions as JS sees them: a value whose type is a union of classes that all are `Error`
//! or extend it (`AppError | TypeError`, a `catch` variable of several error classes) has
//! `Error`'s `name` and `toString()` (each class's own, dispatched), and `String(e)` writes
//! it as `toString()` does; a nullable class value writes `null` (or `undefined`) or its
//! `toString()`.

use velt_common::Span;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, PatKind as P, TyId, TyKind, UseMode};

impl FnCx<'_, '_> {
    /// The prelude `Error` class type.
    fn error_class_ty(&mut self) -> Option<TyId> {
        let d = self.cx.prelude_adt("Error")?;
        Some(self.cx.ty.intern(TyKind::Adt(d, vec![])))
    }

    /// Is `t` a union whose members all are `Error` or extend it?
    pub(crate) fn is_error_union(&mut self, t: TyId) -> bool {
        let Some(err) = self.cx.prelude_adt("Error") else {
            return false;
        };
        if self.cx.union_def(t).is_none() {
            return false;
        }
        let members = self.cx.union_members(t).unwrap_or_default();
        !members.is_empty()
            && members.iter().all(|&m| {
                let mut c = self.cx.class_of(m).map(|(d, _)| d);
                while let Some(d) = c {
                    if d == err {
                        return true;
                    }
                    c = self
                        .cx
                        .adt(d)
                        .and_then(|a| a.base)
                        .and_then(|b| self.cx.class_of(b))
                        .map(|(b, _)| b);
                }
                false
            })
    }

    /// An error union value (`is_error_union`) as an `Error` (`Err` gives other values back).
    pub(crate) fn error_union_view(&mut self, h: hir::Expr) -> Result<hir::Expr, hir::Expr> {
        if !self.is_error_union(h.ty) {
            return Err(h);
        }
        let Some(ety) = self.error_class_ty() else {
            return Err(h);
        };
        self.union_to_common(h, ety)
    }

    /// `e.<prop>` on an error union reads `Error`'s accessor or method `prop` (`name`,
    /// `toString`) through the `Error` view; other members (`message`, a field every member
    /// has) read as on any union.
    pub(crate) fn error_union_accessor(&mut self, h: hir::Expr, prop: &str) -> hir::Expr {
        if !self.is_error_union(h.ty) {
            return h;
        }
        let Some(ety) = self.error_class_ty() else {
            return h;
        };
        if !self.has_getter(ety, prop) && self.resolve_method(ety, prop).is_none() {
            return h;
        }
        self.error_union_view(h).unwrap_or_else(|h| h)
    }

    /// Does `String(x)` of a value of type `t` call a `toString()` (a class with one, own or
    /// inherited, or an error union)?
    pub(crate) fn writes_by_to_string(&mut self, t: TyId) -> bool {
        if self.is_error_union(t) {
            return true;
        }
        let Some((d, args)) = self.adt_of(t) else {
            return false;
        };
        self.cx.class_of(t).is_some()
            && matches!(
                crate::collect::lookup_method(self.cx, d, &args, crate::hooks::TO_STRING),
                Some(crate::collect::Found::Class { .. })
            )
    }

    /// `String(h)` for a nullable `h` whose payload is written by its `toString()`:
    /// `match (h) { Some(v) => String(v), null => "null" }` (`"undefined"` for `T | undefined`);
    /// `Err(h)` for other values.
    pub(crate) fn nullable_text(
        &mut self,
        mut h: hir::Expr,
        what: &str,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some(payload) = self.cx.ty.opt_payload(h.ty) else {
            return Err(h);
        };
        if !self.writes_by_to_string(payload) {
            return Err(h);
        }
        let span: Span = h.span;
        let (l, mode) = self.option_binding(&h, payload, "<text>", false);
        if mode == UseMode::Move {
            self.force_move(&mut h);
        }
        let v = self.mk(H::Local(l, mode), payload, span);
        let some = self.js_text(v, what);
        // A `T | null | undefined`'s own nothing is `undefined` (its payload is the `T | null`).
        let word = match self.cx.ty.nothing_of(h.ty) {
            Some(crate::hir::Nothing::Both) => "undefined",
            n => n.map_or("null", |n| n.word()),
        };
        let none = self.str_lit(word, span);
        let sty = h.ty;
        let bind = self.pat(P::Binding(l, mode), payload, span);
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Some(Box::new(bind)), sty, span),
                guard: None,
                body: some,
            },
            hir::Arm {
                pat: self.pat(P::None, sty, span),
                guard: None,
                body: none,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(kind, self.cx.ty.str_, span))
    }
}
