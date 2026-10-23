//! A dynamic value converted to a union that has `any` or `unknown` among its members: a generic
//! union at such an instantiation (`T | Mod<T>` at `T = unknown`), which stays a union (anon.rs
//! `canon_union`). A value that passes no other member's test is that member's, whole, so the
//! conversion never fails, as in TypeScript, where the union is that `unknown`.

use velt_common::Span;

use crate::any::Dynamic;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, Intrinsic, LocalId, TyId, UseMode};

impl FnCx<'_, '_> {
    /// The member of `members` that takes every value of the dynamic type `dty` whole: an `any`,
    /// or an `unknown` (which takes an `any` too).
    pub(super) fn dynamic_member(&mut self, members: &[TyId], dty: TyId) -> Option<TyId> {
        let from = self.cx.dynamic(dty)?;
        let takes = |d: Option<Dynamic>| match d {
            Some(Dynamic::Any | Dynamic::Unknown) => true,
            Some(d) => d == from,
            None => false,
        };
        members.iter().copied().find(|m| takes(self.cx.dynamic(*m)))
    }

    /// Does the union (or optional union) `exp` take a value of the dynamic type `dty` whole?
    pub(super) fn takes_dynamic_whole(&mut self, exp: TyId, dty: TyId) -> bool {
        let core = self.present_payload(exp).unwrap_or(exp);
        let members = self.cx.union_members(core).unwrap_or_default();
        self.dynamic_member(&members, dty).is_some()
    }

    /// The dynamic value in `tmp` (of type `dty`) as member `m` of union `exp`.
    pub(super) fn whole_as_member(
        &mut self,
        tmp: LocalId,
        dty: TyId,
        m: TyId,
        exp: TyId,
        span: Span,
    ) -> hir::Expr {
        let x = self.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        let share = self.intrinsic(Intrinsic::Share, vec![x], dty, span);
        let value = match dty == m {
            true => share,
            false => self.intrinsic(Intrinsic::AnyTo, vec![share], m, span),
        };
        let core = self.present_payload(exp).unwrap_or(exp);
        let (Some((def, type_args)), Some(members)) =
            (self.cx.union_def(core), self.cx.union_members(core))
        else {
            return self.coerce(value, exp);
        };
        let variant = members.iter().position(|x| *x == m).unwrap_or(0) as u32;
        let kind = H::Variant {
            def,
            type_args,
            variant,
            args: vec![value],
        };
        let value = self.mk(kind, core, span);
        match core == exp {
            true => value,
            false => self.coerce(value, exp),
        }
    }
}
