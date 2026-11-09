//! The "mismatched types" report of a value that does not convert to the type expected
//! (`coerce.rs`), with the notes that say why: a nullable value, a brand, a class instance
//! where a data type is expected, optional fields, an interface the value does not implement.

use velt_common::Diagnostic;

use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, TyId, TyKind};

impl FnCx<'_, '_> {
    pub fn report_mismatch(&mut self, expected: TyId, found: &hir::Expr) {
        // `reject(v)` of a promise without a written error type, a union with a `Promise<T>`
        // member or a closure's `return`: the program is checked again with what was stored
        // there, which drops this report (`throws::promise_sites`).
        self.cx.seed_reject(found.ty, expected);
        self.cx.seed_reject_fn(found.ty, expected);
        self.cx.seed_promise_site(found.ty, expected);
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
                if let Some(note) = self.number_expr_note(expected, found) {
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
        if self.cx.is_generic_fn(expected) && self.cx.is_generic_fn(found.ty) {
            d = d.with_note("its function was checked at the type parameters' bounds of its own type arguments; as a value at other type arguments, whose bounds differ, it is not supported yet (#943)");
        } else if self.cx.is_generic_fn(expected) {
            d = d.with_note(super::generic_conversions::fixed_note(&e, &f));
        } else if let Some(name) = self.bound_only_iface(expected) {
            d = d.with_note(format!(
                "`{name}` is only a bound for type parameters (`T extends {name}`): no value has it as its type"
            ));
        } else if let TyKind::Dyn(..) = self.cx.ty.kind(expected) {
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
