//! Implicit conversions at typed positions (the only ones Velt has):
//! `T` → `T | null` (`WrapSome`), member → union, union → wider union and union → a type every
//! member converts to (`union_coerce`), literal type → its base (`literal_types`), string enum
//! → `string`, subclass → base class (`Upcast`), concrete type → interface value (`ToDyn`,
//! moves the value), `i8`..`i32`, `u8`..`u32` and `f32` → `number` (`expr::numbers`), `never`
//! → anything, and `Promise<T, E1>` → `Promise<T, E2>` when `E2` allows every error of `E1`
//! (`never` included: `Promise<T>` → `Promise<T, E>`; `Intrinsic::PromiseWiden`), `T | null` →
//! `U | null` when `T` converts to `U`, a fresh array or object to a wider one (`widen_fresh`),
//! and an object type to one with some of its fields, or the same ones in another order
//! (`object_copy`).

use velt_common::Diagnostic;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Convert `h` to type `exp`, or report "mismatched types".
    pub fn coerce(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        self.literal_use_as(&h, exp);
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
        if self.cx.brand_base(h.ty).is_none() {
            return self.try_coerce_value(h, exp);
        }
        // A branded value converts as itself (`UserId` to `UserId | null`), else like its
        // primitive (a primitive never converts to a brand).
        match self.try_coerce_value(h, exp) {
            Ok(h) => Ok(h),
            Err(h) => {
                let brand = h.ty;
                let h = self.unbrand(h);
                self.try_coerce(h, exp).map_err(|mut h| {
                    h.ty = brand;
                    h
                })
            }
        }
    }

    fn try_coerce_value(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let t = &self.cx.ty;
        if h.ty == exp
            || t.is_bottom(h.ty)
            || exp == t.error
            || (t.has_error(exp) && self.loosely_equal(exp, h.ty))
        {
            return Ok(h);
        }
        // Two forms of one anonymous object type (a generic shape at concrete arguments and the
        // written shape, crate::anon): the same values, so only the type changes.
        if self.cx.canon(h.ty) == self.cx.canon(exp) {
            let mut h = h;
            h.ty = exp;
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
            if self.cx.ty.opt_payload(h.ty).is_some()
                && (self.cx.union_def(inner).is_some() || !self.cx.same_layout(h.ty, exp))
            {
                return self.option_to_option(h, exp);
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
            return self.upcast(h, exp).or_else(|h| self.widen_fresh(h, exp));
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
        // `i8`..`i32`, `u8`..`u32` and `f32` are numbers too (exactly).
        if exp == self.cx.ty.f64 && self.exact_in_number(h.ty) {
            return Ok(self.int_to_float(h, exp));
        }
        let h = match self.copy_object(h, exp) {
            Ok(h) => return Ok(h),
            Err(h) => h,
        };
        self.widen_fresh(h, exp)
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
        if self.shared_widening_error(expected, found) {
            return;
        }
        let e = self.cx.display(expected);
        let f = self.cx.display(found.ty);
        let mut d = Diagnostic::error("mismatched types", found.span)
            .with_note(format!("expected {e}, found {f}"));
        if self.cx.ty.is_float(expected) && matches!(found.kind, H::Lit(hir::Lit::Int(_))) {
            d = d.with_note(
                "integer literals are not floats; write it with a decimal point, e.g. `1.0`",
            );
        }
        match self
            .float_division_note(found)
            .filter(|_| self.cx.ty.is_int(expected))
        {
            Some(note) => d = d.with_note(note),
            None => {
                if let Some(note) = self.number_note(expected, found.ty) {
                    d = d.with_note(note);
                }
            }
        }
        if let Some(note) = self.narrowing_note(found.ty) {
            d = d.with_note(note);
        }
        if let TyKind::Dyn(..) = self.cx.ty.kind(expected) {
            d = match self.extends_note(expected, found.ty) {
                Some(note) => d.with_note(note),
                None => d.with_note(format!("`{f}` does not declare `implements {e}`")),
            };
            if let Some(note) = self.other_args_note(expected, found.ty) {
                d = d.with_note(note);
            }
        }
        if let Some(base) = self.cx.brand_base(expected) {
            let b = self.cx.display(base);
            let from = match self.cx.brand_base(found.ty) {
                Some(_) => format!("`{}`, another brand,", self.cx.display(found.ty)),
                None => format!("a plain `{b}`"),
            };
            d = d.with_note(format!(
                "`{e}` is a branded `{b}`: {from} does not convert to it; brand a value with `x as {e}`"
            ));
        }
        if let Some(note) = self.class_to_data_note(expected, found) {
            d = d.with_note(format!("`{e}` has only fields, so it is a data type, like `type {e} = {{ … }}`: a class instance is shared by reference and is not one"))
                .with_note(note);
        }
        if self.only_optional_differs(expected, found.ty) {
            d = d.with_note(
                "an optional field (`a?: T`) may be absent, so it is not a `T | null` field: the two object types differ in which fields may be left out; copy the value to convert it: `{ ...x }`",
            );
        } else if e == f && self.is_anon(expected) && self.is_anon(found.ty) {
            // Instances are canonical (crate::anon), except a generic union whose members are
            // themselves unions or nullable: `U | string` at `U = i64 | bool` keeps its own
            // variants (#350).
            d = d.with_note(
                "the two object types look the same but a field's union type was built differently (a generic union instantiated with a union or nullable member is not the written union yet); write the union type the same way in both",
            );
        }
        self.cx.error(d);
    }

    /// For an interface value where an interface it extends is expected (`IterableIterator<T>`
    /// for `Iterator<T>`): such values don't convert yet.
    fn extends_note(&mut self, expected: TyId, found: TyId) -> Option<String> {
        let (TyKind::Dyn(want, _), TyKind::Dyn(have, _)) = (
            self.cx.ty.kind(expected).clone(),
            self.cx.ty.kind(found).clone(),
        ) else {
            return None;
        };
        let parents = self.cx.iface(have)?.parents.clone();
        if !parents.iter().any(|p| p.iface == want) {
            return None;
        }
        let (e, f) = (self.cx.display(expected), self.cx.display(found));
        let fix = match Some(want) == self.cx.prelude_iface("Iterator") {
            true => "; `x[Symbol.iterator]()` gives its `Iterator`".to_string(),
            false => format!(": pass the value it was made from, or take a `{f}`"),
        };
        Some(format!(
            "`{f}` extends `{e}`, but an interface value does not convert to the interfaces it extends yet{fix}"
        ))
    }

    /// For a value implementing the expected interface with other type arguments (`string[]`
    /// where an `Iterable<f64>` is expected): which ones.
    fn other_args_note(&mut self, expected: TyId, found: TyId) -> Option<String> {
        let TyKind::Dyn(want, _) = self.cx.ty.kind(expected).clone() else {
            return None;
        };
        let (_, args, _) = self.cx.find_impl(found, want)?;
        let has = self.cx.ty.intern(TyKind::Dyn(want, args));
        let (e, f, h) = (
            self.cx.display(expected),
            self.cx.display(found),
            self.cx.display(has),
        );
        Some(format!("`{f}` is an `{h}`, not an `{e}`"))
    }

    /// For a class instance where a field-only interface's object type is expected: how to build
    /// one from the instance (copying is explicit, so later writes to the instance are not
    /// silently lost).
    fn class_to_data_note(&mut self, expected: TyId, found: &hir::Expr) -> Option<String> {
        let TyKind::Adt(d, _) = self.cx.ty.kind(expected).clone() else {
            return None;
        };
        self.cx.field_only_of.get(&d)?;
        self.cx.class_of(found.ty)?;
        let src = match &found.kind {
            H::Local(l, _) => self.f.locals[l.0 as usize].name.clone(),
            _ => "x".into(),
        };
        let fields: Vec<String> = self
            .cx
            .adt(d)?
            .fields
            .iter()
            .map(|f| format!("{0}: {src}.{0}", f.name))
            .collect();
        let e = self.cx.display(expected);
        Some(format!(
            "build one from it: `{{ {} }}`, or give `{e}` a method to make it an interface classes implement",
            fields.join(", ")
        ))
    }

    /// Is `t` an anonymous object type (`{ v: number }`, a generic alias's instance)?
    fn is_anon(&self, t: TyId) -> bool {
        matches!(self.cx.ty.kind(t), TyKind::Adt(d, _)
            if self.cx.adt(*d).is_some_and(|a| a.kind == crate::hir::AdtKind::Anon))
    }

    /// Is `found` acceptable where `expected` is required (without conversion)?
    pub fn compatible(&self, expected: TyId, found: TyId) -> bool {
        expected == found || self.cx.ty.is_bottom(found) || expected == self.cx.ty.error
    }
}

impl FnCx<'_, '_> {
    /// `a` and `b` are anonymous object types with the same field names and read types that
    /// differ only in which fields are optional (`{ a?: T }` and `{ a: T | null }`).
    fn only_optional_differs(&mut self, a: TyId, b: TyId) -> bool {
        let (TyKind::Adt(da, aa), TyKind::Adt(db, ab)) =
            (self.cx.ty.kind(a).clone(), self.cx.ty.kind(b).clone())
        else {
            return false;
        };
        if !self.cx.same_anon_shape(da, db) {
            return false;
        }
        let fa: Vec<TyId> = self.cx.anon_field_tys(da);
        let fb: Vec<TyId> = self.cx.anon_field_tys(db);
        let fa: Vec<TyId> = fa.iter().map(|t| self.cx.subst(*t, &aa)).collect();
        let fb: Vec<TyId> = fb.iter().map(|t| self.cx.subst(*t, &ab)).collect();
        let opt = |cx: &crate::ctx::Ctx, d| {
            cx.adt(d)
                .map(|x| x.fields.iter().map(|f| f.optional).collect::<Vec<_>>())
        };
        fa == fb && opt(self.cx, da) != opt(self.cx, db)
    }
}
