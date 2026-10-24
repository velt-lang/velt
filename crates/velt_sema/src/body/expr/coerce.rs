//! Implicit conversions at typed positions (the only ones Velt has):
//! `T` → `T | null` (`WrapSome`), member → union, union → wider union and union → a type every
//! member converts to (`union_coerce`), literal type → its base (`literal_types`), string enum
//! → `string`, subclass → base class (`Upcast`), concrete type → interface value (`ToDyn`,
//! moves the value), `i8`..`i32`, `u8`..`u32` and `f32` → `number` (`expr::numbers`), `never`
//! → anything, and `Promise<T, E1>` → `Promise<T, E2>` when `E2` allows every error of `E1`
//! (`never` included: `Promise<T>` → `Promise<T, E>`; `Intrinsic::PromiseWiden`), `T | null` →
//! `U | null` when `T` converts to `U`, a fresh array or object to a wider one (`widen_fresh`),
//! an object type to one with some of its fields, or the same ones in another order
//! (`object_copy`), and a generic interface value to the same interface with `unknown` or `any`
//! type arguments (`iface_adapt`), and a `shared<T>` to the `T` it holds, read in place
//! (`frozen`).

use velt_common::Diagnostic;

use crate::body::FnCx;
use crate::hir::{self, Callee, ExprKind as H, Nothing, TyId, TyKind};

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
    pub fn try_coerce(&mut self, mut h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        // A record is a view of itself with named members (`crate::index_signatures`).
        if self
            .cx
            .record_view(exp)
            .is_some_and(|(base, _)| base == h.ty)
        {
            h.ty = exp;
            return Ok(h);
        }
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
        let h = match self.fn_bound_value(h) {
            Ok(f) if f.ty == exp || matches!(self.cx.ty.kind(exp), TyKind::FnPtr { .. }) => {
                return self.try_coerce_value(f, exp)
            }
            Ok(f) => match &f.kind {
                H::Upcast(inner) => (**inner).clone(),
                _ => f,
            },
            Err(h) => h,
        };
        let h = match self.try_coerce_typed(h, exp) {
            Ok(h) => return Ok(h),
            Err(h) => h,
        };
        // Types that convert as written, but for the error-set variables in them
        // (`throws::vars`): converted as written, assuming the variables empty; when one is
        // not, the program is checked again with the solved sets.
        if !self.cx.fn_sites.use_vars
            || !(self.cx.mentions_err_var(h.ty) || self.cx.mentions_err_var(exp))
        {
            return Err(h);
        }
        let (from, to) = (self.cx.plain_type(h.ty), self.cx.plain_type(exp));
        let ty = h.ty;
        let mut p = h;
        p.ty = from;
        match self.try_coerce_typed(p, to) {
            Ok(mut c) => {
                self.cx.assume_empty_in(ty);
                self.cx.assume_empty_in(exp);
                c.ty = exp;
                Ok(c)
            }
            Err(mut p) => {
                p.ty = ty;
                Err(p)
            }
        }
    }

    fn try_coerce_typed(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let t = &self.cx.ty;
        if h.ty == exp
            || t.is_bottom(h.ty)
            || exp == t.error
            || (t.has_error(exp) && self.loosely_equal(exp, h.ty))
        {
            return Ok(h);
        }
        // Types equal but for their error-set variables (`throws::vars`).
        if self.cx.fn_sites.use_vars
            && (self.cx.unify_err_vars(h.ty, exp) || self.cx.unify_ctor_vars(h.ty, exp))
        {
            let mut h = h;
            h.ty = exp;
            return Ok(h);
        }
        if self.cx.unfreezes(h.ty, exp) {
            return Err(h);
        }
        // A `shared<T>` where a `T` is expected: the value inside, read (`frozen`, #36).
        if matches!(self.cx.ty.kind(h.ty), TyKind::Shared(t) if *t == exp) {
            let v = self.frozen_view(h, None);
            return match Self::is_frozen_view(&v) {
                true => Ok(v),
                false => Err(v),
            };
        }
        // A function with properties as its plain function type: its function (#921).
        if self.cx.is_callable_object(h.ty) && matches!(self.cx.ty.kind(exp), TyKind::FnPtr { .. })
        {
            let f = self.callable_as_fn(h);
            return self.try_coerce(f, exp);
        }
        if self.cx.dynamic(h.ty).is_some() || self.cx.dynamic(exp).is_some() {
            return self.coerce_dynamic(h, exp, self.adapting);
        }
        // `Record<string, any>` as a `Record<string, unknown>` (or back), also in arrays,
        // nullables, maps and function types: the same value (`Ctx::same_but_any_unknown`).
        if self.cx.same_but_any_unknown(h.ty, exp) {
            self.cx.any_unknown_views = true;
            let span = h.span;
            return Ok(self.mk(H::Upcast(Box::new(h)), exp, span));
        }
        // Two forms of one anonymous object type (a generic shape at concrete arguments and the
        // written shape, crate::anon): the same values, so only the type changes.
        if self.cx.canon(h.ty) == self.cx.canon(exp) {
            let mut h = h;
            h.ty = exp;
            return Ok(h);
        }
        // `T & object` and `T` at a `T extends object` (crate::intersections).
        if self.same_under_bounds(h.ty, exp) {
            let mut h = h;
            h.ty = exp;
            return Ok(h);
        }
        // `Generator<T, E, N>` to `Generator<T, E>` or back: `N` only types `next(value)`,
        // and one side takes no value (the other never reads one).
        if self.cx.generator_next_differs(h.ty, exp) {
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
        if let Some(widens) = self.cx.fn_throws_differ(h.ty, exp) {
            return match widens {
                true => self.adapt_fn(h, exp),
                false => Err(h),
            };
        }
        // A function that never returns (`() => { throw e; }`, typed `() => never`) as a
        // function of another result type (`() => void`, `() => number`), as `never` converts
        // to any type: wrapped, the wrapper typed by `exp` (its errors go where `exp`'s do).
        if let Some(void) = self.never_result_converts(h.ty, exp) {
            // As a function returning nothing it is the same function (it never returns): its
            // type changes, and its errors convert as any function value's do.
            if let Some(void) = void {
                let mut h = h;
                h.ty = void;
                return if void == exp {
                    Ok(h)
                } else {
                    self.try_coerce(h, exp)
                };
            }
            if !self.frameless() {
                return self.adapt_fn(h, exp);
            }
        }
        if self.converts_by_state(h.ty, exp) {
            return self.coerce_hidden_nothings(h, exp);
        }
        // `T | undefined` to `T | null` (or the reverse): TS2322, as in TypeScript; converting
        // would turn one nothing into the other (issue #930).
        if let (Some(found), Some(want)) = (self.cx.ty.nothing_of(h.ty), self.cx.ty.nothing_of(exp))
        {
            if !found.within(want) {
                return Err(h);
            }
        }
        // `undefined` alone (an option of `never`): always its nothing, which is `exp`'s.
        if let (Some(n), Some(_)) = (self.cx.ty.nothing_of(h.ty), self.cx.ty.nothing_of(exp)) {
            if self.cx.ty.core(h.ty) == Some(self.cx.ty.never) {
                let span = h.span;
                let value = self.nothing_value(n, exp, span);
                if matches!(h.kind, H::Lit(_)) {
                    return Ok(value);
                }
                let block = hir::Block {
                    stmts: vec![hir::Stmt {
                        kind: hir::StmtKind::Expr(h),
                        span,
                    }],
                    value: Some(Box::new(value)),
                    span,
                };
                return Ok(self.mk(H::Block(block), exp, span));
            }
        }
        // Into a `T | null | undefined` (an option of a `T | null`): a `T | null` is its
        // payload, a `T | undefined` moves its nothing (hir_encodings.md "Undefined").
        if self.is_three_state(exp) && !self.is_three_state(h.ty) {
            match self.cx.ty.nothing_of(h.ty) {
                Some(Nothing::Undefined) => {
                    let core = self.cx.ty.core(exp).expect("ICE: option without a payload");
                    let undef = self.cx.ty.option_of(core, Nothing::Undefined);
                    let h = self.try_coerce(h, undef)?;
                    return Ok(self.undefined_to_three_state(h, exp));
                }
                Some(_) => {
                    let mid = self
                        .cx
                        .ty
                        .opt_payload(exp)
                        .expect("ICE: option without a payload");
                    let span = h.span;
                    let h = self.try_coerce(h, mid)?;
                    return Ok(self.mk(H::WrapSome(Box::new(h)), exp, span));
                }
                None => {}
            }
        }
        if let Some(inner) = self.cx.ty.opt_payload(exp) {
            if self.cx.ty.opt_payload(h.ty).is_some()
                && (self.cx.union_def(inner).is_some() || !self.cx.same_layout(h.ty, exp))
            {
                // A `U | null` local narrowed to some members of `U` (`typeof x !== "number"`)
                // converts as the narrowed union: the other variants are unreachable.
                let members = self.narrowed_variants(&h);
                return self.option_to_option_narrowed(h, exp, members);
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
        if self.cx.same_layout(h.ty, exp) {
            // Object types that differ only in `readonly`, or containers of a base and its
            // extension (`collect::extended_layouts`, also `Map`s): the same object, seen through
            // the other type (`crate::readonly` makes them one type before lowering).
            let span = h.span;
            return Ok(self.mk(H::Upcast(Box::new(h)), exp, span));
        }
        if self.cx.class_of(exp).is_some() && self.cx.class_of(h.ty).is_some() {
            return self.upcast(h, exp).or_else(|h| self.widen_fresh(h, exp));
        }
        if let TyKind::Dyn(iface, args) = self.cx.ty.kind(exp).clone() {
            return self.dyn_value(h, exp, iface, &args);
        }
        // `i8`..`i32`, `u8`..`u32` and `f32` are numbers too (exactly).
        if exp == self.cx.ty.f64 && self.exact_in_number(h.ty) {
            return Ok(self.int_to_float(h, exp));
        }
        if self.adapting
            && matches!(self.cx.ty.kind(h.ty), TyKind::FnPtr { .. })
            && matches!(self.cx.ty.kind(exp), TyKind::FnPtr { .. })
        {
            return self.adapt_fn(h, exp);
        }
        if self.adapting
            && matches!(self.cx.ty.kind(h.ty), TyKind::Array(_))
            && matches!(self.cx.ty.kind(exp), TyKind::Array(_))
        {
            return self.adapt_array(h, exp);
        }
        if self.drops_void_params(h.ty, exp) {
            // `(value: void) => void` where `() => void` is expected (a `Promise<void>`'s
            // `resolve` stored in a `() => void`): TypeScript lets a trailing `void`
            // parameter be left out, so a wrapper calls it without the argument.
            return self.adapt_fn(h, exp);
        }
        let h = match self.deferred_convert(h, exp) {
            Ok(h) => return Ok(h),
            Err(h) => h,
        };
        let h = match self.copy_object(h, exp) {
            Ok(h) => return Ok(h),
            Err(h) => h,
        };
        // `(v: any) => …` as a `(x: number) => void`: wrapped, converting each argument.
        self.widen_fresh(h, exp)
            .or_else(|h| self.widened_params(h, exp))
    }

    /// Types equal up to `Error` components (an expected type with unknown parts).
    fn loosely_equal(&self, exp: TyId, found: TyId) -> bool {
        let (a, b) = (self.cx.ty.kind(exp).clone(), self.cx.ty.kind(found).clone());
        match (a, b) {
            (TyKind::Error, _) => true,
            (TyKind::Array(x), TyKind::Array(y)) | (TyKind::Option(x, _), TyKind::Option(y, _)) => {
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
                    rest: rest_a,
                },
                TyKind::FnPtr {
                    params: q,
                    ret: s,
                    throws: u,
                    rest: rest_b,
                },
            ) => {
                p.len() == q.len()
                    && rest_a == rest_b
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
        // `I<A>` where `I<unknown>` is expected: wrapped (`iface_adapt`).
        if let TyKind::Dyn(have, from) = self.cx.ty.kind(h.ty).clone() {
            if have == iface && self.cx.dynamic(h.ty).is_none() {
                return self.adapt_iface(h, exp, iface, &from, args);
            }
            // An interface value where an interface whose members it has is expected (a part
            // of an intersection, a `Pick` of it, an interface it extends): the same object
            // (`crate::synth_ifaces`).
            if self.cx.dynamic(h.ty).is_none()
                && self.cx.dynamic(exp).is_none()
                && self.cx.iface_upcast((have, &from), (iface, args))
            {
                let mut h = h;
                self.force_move(&mut h);
                let span = h.span;
                return Ok(self.mk(H::Upcast(Box::new(h)), exp, span));
            }
        }
        let Some((index, iargs, imp_ty)) = self.cx.find_impl(h.ty, iface) else {
            return Err(h);
        };
        // Arguments that differ only in their error-set variables are one (`throws::vars`).
        if iargs != args && !self.cx.unify_err_var_lists(&iargs, args) {
            // A value implementing `I<A>`: an `I<A>` first, then adapted.
            if !self.iface_adapts(iface, &iargs, args) {
                return Err(h);
            }
            let mid = self.cx.ty.intern(TyKind::Dyn(iface, iargs.clone()));
            let h = self.dyn_value(h, mid, iface, &iargs)?;
            return self.adapt_iface(h, exp, iface, &iargs, args);
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

    /// Is `h` the result of a standard library function or method (`Channel.receive`)?
    pub(crate) fn std_result(&self, h: &hir::Expr) -> bool {
        match &h.kind {
            H::Call {
                callee: Callee::Def(d, _),
                ..
            } => self
                .cx
                .try_fn(*d)
                .is_some_and(|fi| self.cx.scopes[fi.module].is_std),
            H::Call {
                callee: Callee::Intrinsic(_),
                ..
            } => true,
            H::Block(b) => b.value.as_deref().is_some_and(|v| self.std_result(v)),
            H::Await(x) => self.std_result(x),
            _ => false,
        }
    }

    pub fn report_mismatch(&mut self, expected: TyId, found: &hir::Expr) {
        // The comparisons don't see variables in the fields of anonymous object types yet
        // (`throws::vars`): such a mismatch may come from them.
        if self.cx.anon_vars_may_mismatch(expected, found.ty) {
            self.cx.fn_sites.set_doubt();
        }
        if self.cx.dynamic(found.ty) == Some(crate::any::Dynamic::Unknown) {
            return self.unknown_use_error(found.span);
        }
        if self.shared_widening_error(expected, found) {
            return;
        }
        let undefined = |t: TyId| self.cx.ty.nothing_of(t).is_some_and(|n| n.has_undefined());
        if undefined(found.ty) && !undefined(expected) && self.optional_slot_error(found.span, true)
        {
            return;
        }
        let e = self.cx.display(expected);
        let f = self.cx.display(found.ty);
        if self.throws_where_none_may(expected, found, &e, &f) {
            return;
        }
        if let Some(note) = self.field_throws_note(expected, found.ty) {
            self.cx.error(
                Diagnostic::error("mismatched types", found.span)
                    .with_note(format!("expected {e}, found {f}"))
                    .with_note(note),
            );
            return;
        }
        if self.cx.unfreezes(found.ty, expected) {
            self.cx.error(
                Diagnostic::error(
                    format!("an `Object.freeze` result where a `{e}` is expected is not supported yet: its fields could be written there"),
                    found.span,
                )
                .with_note("Node throws a `TypeError` when a frozen object's field is written, which Velt checks while compiling: pass it where a `Readonly<T>` is expected"),
            );
            return;
        }
        if let (Some(have), Some(want)) = (
            self.cx.ty.nothing_of(found.ty),
            self.cx.ty.nothing_of(expected),
        ) {
            if !have.within(want) {
                let (x, y) = (have.word(), want.word());
                self.cx.error(
                    Diagnostic::error(
                        format!("type `{f}` is not assignable to type `{e}` (TypeScript's TS2322)"),
                        found.span,
                    )
                    .with_note(format!(
                        "`{x}` is not `{y}`; convert it with `?? {y}`, or change the type"
                    )),
                );
                return;
            }
        }
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
        if matches!(self.cx.ty.kind(found.ty), TyKind::Shared(t) if *t == expected) {
            d = d.with_note(self.unreadable_shared_note(found.ty));
        }
        if let TyKind::Dyn(..) = self.cx.ty.kind(expected) {
            d = match (
                self.adapt_refusal(expected, found.ty),
                self.extends_note(expected, found.ty),
            ) {
                (Some(why), _) => d.with_note(format!(
                    "`{f}` converts to `{e}` through a wrapper that checks each call's arguments, but {why}"
                )),
                (None, Some(note)) => d.with_note(note),
                (None, None) => d.with_note(format!("`{f}` does not declare `implements {e}`")),
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
        if let Some(note) = self.cx.object_part_note(expected, found.ty) {
            d = d.with_note(note);
        }
        if self.only_optional_differs(expected, found.ty) {
            d = d.with_note(
                "an optional field (`a?: T`) may be absent, so it is not a `T | undefined` field: the two object types differ in which fields may be left out; copy the value to convert it: `{ ...x }`",
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

impl FnCx<'_, '_> {
    /// Is `from` a function type returning `never` with the parameters of function type `to`,
    /// which returns something else? `Some(Some(f))` when `to` returns nothing: `f` is `from`
    /// returning nothing (with the errors of `from`).
    fn never_result_converts(&mut self, from: TyId, to: TyId) -> Option<Option<TyId>> {
        let (
            TyKind::FnPtr {
                params: fp,
                ret: fr,
                rest: fx,
                ..
            },
            TyKind::FnPtr {
                params: tp,
                ret: tr,
                rest: tx,
                ..
            },
        ) = (self.cx.ty.kind(from), self.cx.ty.kind(to))
        else {
            return None;
        };
        if fp != tp || fx != tx || *fr != self.cx.ty.never || tr == fr {
            return None;
        }
        let (params, rest, ret) = (fp.clone(), *fx, *tr);
        let TyKind::FnPtr { throws, .. } = *self.cx.ty.kind(from) else {
            return None;
        };
        if ret != self.cx.ty.unit {
            return Some(None);
        }
        let void = self.cx.ty.intern(TyKind::FnPtr {
            params,
            ret,
            throws,
            rest,
        });
        Some(Some(void))
    }

    /// Is function type `from` function type `to` with trailing `void` parameters added?
    fn drops_void_params(&self, from: TyId, to: TyId) -> bool {
        let (
            TyKind::FnPtr {
                params: fp,
                ret: fr,
                throws: ft,
                rest: false,
            },
            TyKind::FnPtr {
                params: tp,
                ret: tr,
                throws: tt,
                rest: false,
            },
        ) = (self.cx.ty.kind(from), self.cx.ty.kind(to))
        else {
            return false;
        };
        fp.len() > tp.len()
            && fp[..tp.len()] == tp[..]
            && fp[tp.len()..].iter().all(|&p| p == self.cx.ty.unit)
            && fr == tr
            && ft == tt
    }
}
