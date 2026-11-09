//! Narrowing a union local by a test of one of its fields, as TypeScript narrows by a
//! discriminant property (`crate::discriminants`):
//! - `x.kind === "circle"` where the members' `kind` fields are literals;
//! - `if (r.done)` / `if (!cell.ok)` where they are `bool` literals, or where some members' field
//!   is `undefined` or `null` and the others' is always truthy (`{ issues: E[] } | { value: T;
//!   issues?: undefined }`, `if (result.issues)`);
//! - `r.issues === undefined` / `!= null` on such a union.
//!
//! Only the members the local may still hold count: `cell === RESOLVING` ruled out the symbol
//! member, so `cell.ok` discriminates what is left (`{ ok: true; … } | { ok: false; … }`).

use velt_syntax::ast;

use super::narrow::Fact;
use super::FnCx;
use crate::hir::{LitValue, LocalId, Nothing, TyId, TyKind};

/// What a member's field says about a test of the field.
enum Disc {
    /// A literal type: the field always holds this value.
    Lit(LitValue),
    /// `undefined` / `null` alone (`issues?: undefined`).
    Nothing(Nothing),
    /// A type whose values are all truthy objects (`E[]`, an object type, a function).
    Object,
}

/// A union local tested through one of its fields: the local, whether it is a `U | null`, and
/// each member's [`Disc`] (`None` for a member the local can no longer hold).
type Discs = (LocalId, bool, Vec<Option<Disc>>);

impl FnCx<'_, '_> {
    /// The members of the union local behind `e` (`x.prop`) by what their field `prop` holds,
    /// when each member the local may still hold has the field with a [`Disc`] type.
    fn field_discs(&mut self, e: &ast::Expr) -> Option<Discs> {
        let (object, prop) = super::expr::member::member_view(e)?;
        // Each class's `#kind` is its own member: never a discriminant shared by a union.
        if prop.name.starts_with(ast::PRIVATE_NAME_PREFIX) {
            return None;
        }
        let l = self.named_local(object)?;
        let ty = self.local_ty(l);
        let nullable = self.cx.ty.opt_payload(ty).is_some();
        let u = self.cx.ty.core(ty).unwrap_or(ty);
        let members = self.cx.union_members(u)?;
        let live = self.allowed_members(l);
        let mut out = vec![];
        for (v, m) in members.into_iter().enumerate() {
            if live.as_ref().is_some_and(|vs| !vs.contains(&(v as u32))) {
                out.push(None);
                continue;
            }
            let fty = self.cx.member_field_ty(m, &prop.name)?;
            out.push(Some(self.disc_of(fty)?));
        }
        Some((l, nullable, out))
    }

    fn disc_of(&mut self, t: TyId) -> Option<Disc> {
        if let Some(v) = self.cx.lit_value(t) {
            return Some(Disc::Lit(v));
        }
        if self.cx.ty.opt_payload(t) == Some(self.cx.ty.never) {
            return self.cx.ty.nothing_of(t).map(Disc::Nothing);
        }
        let object = match self.cx.ty.kind(t) {
            TyKind::Adt(..) => self.cx.union_def(t).is_none() && self.cx.dynamic(t).is_none(),
            TyKind::Array(_) | TyKind::Map(..) | TyKind::Tuple(_) => true,
            TyKind::FnPtr { .. } | TyKind::Closure(_) => true,
            TyKind::Dyn(..) => self.cx.iface_member(t).is_some(),
            _ => false,
        };
        object.then_some(Disc::Object)
    }

    /// Splits the live members of `discs` by `test` (true / false), as `Members` facts.
    fn split_discs(discs: Discs, test: impl Fn(&Disc) -> bool) -> (Vec<Fact>, Vec<Fact>) {
        let (l, nullable, discs) = discs;
        let (mut yes, mut no) = (vec![], vec![]);
        for (v, d) in discs.iter().enumerate() {
            match d {
                Some(d) if test(d) => yes.push(v as u32),
                Some(_) => no.push(v as u32),
                None => {}
            }
        }
        let mut t = vec![Fact::Members(l, yes)];
        if nullable {
            // `x.kind` could only be read once `x` was known to be non-null.
            t.push(Fact::NonNull(l));
        }
        (t, vec![Fact::Members(l, no)])
    }

    /// Facts of `x.kind == lit` on a discriminated union local `x`: the members whose
    /// discriminant is `lit` / the others.
    pub(super) fn discriminant_facts(
        &mut self,
        e: &ast::Expr,
        lit: &ast::SignedLit,
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let discs = self.field_discs(e)?;
        // A literal discriminant: every member the local may hold has a literal field.
        if !discs.2.iter().flatten().all(|d| matches!(d, Disc::Lit(_))) {
            return None;
        }
        Some(Self::split_discs(discs, |d| match d {
            Disc::Lit(v) => crate::literals::lit_matches(v, lit),
            _ => false,
        }))
    }

    /// Facts of `if (x.prop)` on a union local whose members tell the field's truthiness apart:
    /// `bool` literals (`r.done`), or a nothing in some members and a truthy object in the
    /// others (`result.issues`).
    pub(super) fn truthy_discriminant(&mut self, e: &ast::Expr) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let discs = self.field_discs(e)?;
        let live = || discs.2.iter().flatten();
        let bools = live().all(|d| matches!(d, Disc::Lit(LitValue::Bool(_))));
        let presence = live().all(|d| matches!(d, Disc::Nothing(_) | Disc::Object))
            && live().any(|d| matches!(d, Disc::Nothing(_)));
        if !bools && !presence {
            return None;
        }
        Some(Self::split_discs(discs, |d| {
            matches!(d, Disc::Lit(LitValue::Bool(true)) | Disc::Object)
        }))
    }

    /// Facts of `x.prop === undefined` (`word`; `==` when not `strict`) on a union local whose
    /// members' field is a nothing or a truthy object.
    pub(super) fn nothing_discriminant(
        &mut self,
        e: &ast::Expr,
        word: &str,
        strict: bool,
    ) -> Option<(Vec<Fact>, Vec<Fact>)> {
        let discs = self.field_discs(e)?;
        let live = || discs.2.iter().flatten();
        if !live().all(|d| matches!(d, Disc::Nothing(_) | Disc::Object))
            || !live().any(|d| matches!(d, Disc::Nothing(_)))
        {
            return None;
        }
        let tested = if word == "null" {
            Nothing::Null
        } else {
            Nothing::Undefined
        };
        // A member whose field is the other nothing is in neither branch of a strict test it
        // could only fail: keep the test to the members it splits exactly.
        if strict && live().any(|d| matches!(d, Disc::Nothing(n) if *n != tested)) {
            return None;
        }
        Some(Self::split_discs(discs, |d| matches!(d, Disc::Nothing(_))))
    }
}
