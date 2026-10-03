//! Implicit conversions at typed positions (the only ones Velt has):
//! `T` → `T | null` (`WrapSome`), member → union, union → wider union and union → a type every
//! member converts to (`union_coerce`), literal type → its base (`literal_types`), string enum
//! → `string`, subclass → base class (`Upcast`), concrete type → interface value (`ToDyn`,
//! moves the value), inferred integer → float (`expr::numbers`), `never` → anything, and
//! `Promise<T, E1>` → `Promise<T, E2>` when `E2` allows every error of `E1` (`never` included:
//! `Promise<T>` → `Promise<T, E>`; `Intrinsic::PromiseWiden`).

use velt_common::Diagnostic;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Convert `h` to type `exp`, or report "mismatched types".
    pub fn coerce(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        match self.try_coerce(h, exp) {
            Ok(h) => h,
            Err(h) => {
                self.report_mismatch(exp, &h);
                h
            }
        }
    }

    /// Like [`coerce`](Self::coerce) without reporting; `Err` gives the expression back.
    pub fn try_coerce(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let t = &self.cx.ty;
        if h.ty == exp
            || t.is_bottom(h.ty)
            || exp == t.error
            || (t.has_error(exp) && self.loosely_equal(exp, h.ty))
        {
            return Ok(h);
        }
        if self.widens_promise(h.ty, exp) {
            let span = h.span;
            let call = H::Call {
                callee: hir::Callee::Intrinsic(hir::Intrinsic::PromiseWiden),
                args: vec![h],
            };
            return Ok(self.mk(call, exp, span));
        }
        if let Some(inner) = self.cx.ty.opt_payload(exp) {
            if self.cx.ty.opt_payload(h.ty).is_some() && self.cx.union_def(inner).is_some() {
                return self.option_to_union(h, exp);
            }
            if self.cx.ty.opt_payload(h.ty).is_none() {
                let span = h.span;
                let inner_h = self.try_coerce(h, inner)?;
                return Ok(self.mk(H::WrapSome(Box::new(inner_h)), exp, span));
            }
        }
        if self.cx.union_def(exp).is_some() {
            return self.coerce_to_union(h, exp);
        }
        if self.cx.lit_value(exp).is_some() {
            // Only the same literal converts to a literal type (checked above).
            return Err(h);
        }
        if let Some(v) = self.cx.lit_value(h.ty) {
            let b = self.lit_to_base(h, &v);
            return if b.ty == exp {
                Ok(b)
            } else {
                self.try_coerce(b, exp)
            };
        }
        if self.cx.union_def(h.ty).is_some() {
            return self.union_to_common(h, exp);
        }
        if exp == self.cx.ty.str_ && self.is_string_enum(h.ty) {
            return Ok(self.string_enum_to_str(h));
        }
        if self.cx.class_of(exp).is_some() && self.cx.class_of(h.ty).is_some() {
            return self.upcast(h, exp);
        }
        if self.cx.same_layout(h.ty, exp) {
            // Object types that differ only in `readonly`: the same object, seen through the
            // other type (`crate::readonly` makes them one type before lowering).
            let span = h.span;
            return Ok(self.mk(H::Upcast(Box::new(h)), exp, span));
        }
        if let TyKind::Dyn(iface, args) = self.cx.ty.kind(exp).clone() {
            return self.dyn_value(h, exp, iface, &args);
        }
        if self.cx.ty.is_float(exp) && self.is_inferred_int(&h) {
            return Ok(self.int_to_float(h, exp));
        }
        // A JS number held as an integer adapts to the integer type expected (`s.slice(0,
        // s.length - 1)`, where `slice` takes `i64` and the length is a `usize`).
        let inferred = self.int_origin(&h) == super::numbers::IntOrigin::Inferred;
        if self.cx.ty.is_int(exp) && self.cx.ty.is_int(h.ty) && inferred {
            return Ok(self.int_as(h, exp));
        }
        Err(h)
    }

    /// Types equal up to `Error` components (an expected type with unknown parts).
    fn loosely_equal(&self, exp: TyId, found: TyId) -> bool {
        let (a, b) = (self.cx.ty.kind(exp).clone(), self.cx.ty.kind(found).clone());
        match (a, b) {
            (TyKind::Error, _) => true,
            (TyKind::Array(x), TyKind::Array(y)) | (TyKind::Option(x), TyKind::Option(y)) => {
                self.loosely_equal(x, y)
            }
            (TyKind::Adt(d, xs), TyKind::Adt(e, ys)) | (TyKind::Dyn(d, xs), TyKind::Dyn(e, ys)) => {
                d == e
                    && xs.len() == ys.len()
                    && xs.iter().zip(&ys).all(|(x, y)| self.loosely_equal(*x, *y))
            }
            (
                TyKind::FnPtr {
                    params: p,
                    ret: r,
                    throws: t,
                },
                TyKind::FnPtr {
                    params: q,
                    ret: s,
                    throws: u,
                },
            ) => {
                p.len() == q.len()
                    && p.iter().zip(&q).all(|(x, y)| self.loosely_equal(*x, *y))
                    && self.loosely_equal(r, s)
                    && self.loosely_equal(t, u)
            }
            (TyKind::Promise(x, e), TyKind::Promise(y, f)) => {
                self.loosely_equal(x, y) && self.loosely_equal(e, f)
            }
            (x, y) => x == y,
        }
    }

    /// `Promise<T, E1>` → `Promise<T, E2>`: the same `T` (the wrapper copies the value as it is),
    /// and `E2` allows every error `E1` can reject with (an unrelated error type is still a
    /// mismatch).
    fn widens_promise(&mut self, from: TyId, to: TyId) -> bool {
        let (&TyKind::Promise(x, e), &TyKind::Promise(y, f)) =
            (self.cx.ty.kind(from), self.cx.ty.kind(to))
        else {
            return false;
        };
        x == y && e != f && self.cx.error_outside(Some(f), Some(e)).is_none()
    }

    fn upcast(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let mut cur = h.ty;
        for _ in 0..64 {
            match self.cx.base_of(cur) {
                Some(b) if b == exp => {
                    let span = h.span;
                    return Ok(self.mk(H::Upcast(Box::new(h)), exp, span));
                }
                Some(b) => cur = b,
                None => break,
            }
        }
        Err(h)
    }

    fn dyn_value(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        iface: hir::DefId,
        args: &[TyId],
    ) -> Result<hir::Expr, hir::Expr> {
        let Some((index, iargs, imp_ty)) = self.cx.find_impl(h.ty, iface) else {
            return Err(h);
        };
        if iargs != args {
            return Err(h);
        }
        let mut h = if imp_ty != h.ty {
            self.upcast(h, imp_ty)?
        } else {
            h
        };
        self.force_move(&mut h);
        let span = h.span;
        Ok(self.mk(
            H::ToDyn {
                expr: Box::new(h),
                impl_index: index,
            },
            exp,
            span,
        ))
    }

    pub fn report_mismatch(&mut self, expected: TyId, found: &hir::Expr) {
        let e = self.cx.display(expected);
        let f = self.cx.display(found.ty);
        let mut d = Diagnostic::error("mismatched types", found.span)
            .with_note(format!("expected {e}, found {f}"));
        if self.cx.ty.is_float(expected) && matches!(found.kind, H::Lit(hir::Lit::Int(_))) {
            d = d.with_note(
                "integer literals are not floats; write it with a decimal point, e.g. `1.0`",
            );
        }
        if self.cx.ty.is_int(expected) {
            if let Some(note) = self.float_division_note(found) {
                d = d.with_note(note);
            }
        }
        if let Some(note) = self.narrowing_note(found.ty) {
            d = d.with_note(note);
        }
        if let TyKind::Dyn(..) = self.cx.ty.kind(expected) {
            d = d.with_note(format!("`{f}` does not declare `implements {e}`"));
        }
        self.cx.error(d);
    }

    /// Is `found` acceptable where `expected` is required (without conversion)?
    pub fn compatible(&self, expected: TyId, found: TyId) -> bool {
        expected == found || self.cx.ty.is_bottom(found) || expected == self.cx.ty.error
    }
}
