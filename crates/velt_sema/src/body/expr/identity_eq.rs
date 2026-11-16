//! `a === b` between values of two object types, as TypeScript compares them when one type is
//! assignable to the other.

use crate::body::FnCx;
use crate::hir::{self, DefId, TyId, TyKind};

impl FnCx<'_, '_> {
    /// `a === b` between an interface value and a class or struct value (or a base and a
    /// subclass value) whose types overlap, as TypeScript allows (#365): the side that converts to the other's type
    /// does (an interface value points at the object itself), so the two compare by identity.
    pub(super) fn identity_operands(
        &mut self,
        l: hir::Expr,
        r: hir::Expr,
    ) -> (hir::Expr, hir::Expr) {
        let object = |s: &Self, t: TyId| {
            let t = s.cx.ty.core(t).unwrap_or(t);
            s.cx.union_def(t).is_none()
                && matches!(s.cx.ty.kind(t), TyKind::Adt(..) | TyKind::Dyn(..))
        };
        // `xs === ys` of an `unknown[]` and an `any[]`: one type (`Ctx::same_but_any_unknown`).
        if self.cx.same_but_any_unknown(r.ty, l.ty) {
            let to = l.ty;
            return match self.try_coerce(r, to) {
                Ok(r) | Err(r) => (l, r),
            };
        }
        // Two function types: one converts to the other, and the wrapper the conversion makes
        // is the function it wraps (`hir::Program::fn_wrappers`), as in JavaScript.
        let func = |s: &Self, t: TyId| matches!(s.cx.ty.kind(t), TyKind::FnPtr { .. });
        let funcs = func(self, l.ty) && func(self, r.ty);
        if l.ty == r.ty || !(funcs || object(self, l.ty) && object(self, r.ty)) {
            return (l, r);
        }
        let to = l.ty;
        match self.try_coerce(r, to) {
            Ok(r) => (l, r),
            Err(r) => {
                let to = r.ty;
                let l = self.try_coerce(l, to).unwrap_or_else(|l| l);
                (l, r)
            }
        }
    }

    /// Are `a` and `b` (or what they hold besides `null`/`undefined`) two interfaces, one
    /// extending the other (`Node` and `Comment`)? Such values don't convert to each other
    /// implicitly (`iface_notes`); `as` checks one at run time ([`Self::related_iface_cast`]), and
    /// `===` compares them as `any` values: the object each points at.
    pub(super) fn related_ifaces(&self, a: TyId, b: TyId) -> bool {
        let iface = |t: TyId| {
            let t = self.cx.ty.core(t).unwrap_or(t);
            match self.cx.ty.kind(t) {
                TyKind::Dyn(d, _) if self.cx.union_def(t).is_none() => Some(*d),
                _ => None,
            }
        };
        let (Some(x), Some(y)) = (iface(a), iface(b)) else {
            return false;
        };
        x != y && (self.iface_extends(x, y, 0) || self.iface_extends(y, x, 0))
    }

    /// Does interface `d` extend `base`, directly or through others?
    fn iface_extends(&self, d: DefId, base: DefId, depth: u32) -> bool {
        let Some(i) = self.cx.iface(d) else {
            return false;
        };
        depth < 64
            && i.parents
                .iter()
                .any(|p| p.iface == base || self.iface_extends(p.iface, base, depth + 1))
    }

    /// `e as B` for an `A` value where `B` extends `A` (`shape as Named`), which `tsc` allows since
    /// one is assignable to the other: the value as an `any`, converted to `B` (`Err`: not such a
    /// pair). The conversion checks at run time that the object implements `B`, and a value that
    /// doesn't stops the program with a `TypeError` (#379). Node goes on there and fails only when
    /// a member `B` adds is used; so outside std's `lib.dom` (whose values a server never holds) `B`
    /// must add a required member, which such a value lacks. A `B` that adds only optional ones
    /// stays `cannot cast`: Node adds them when written, and a Velt object has no room for them.
    /// For `lib.dom`, `B` may be a union of such interfaces (`dom as HTMLInputElement |
    /// HTMLTextAreaElement`): the value converts to the first, as the member of the union it is
    /// then ([`Self::related_dom_union`]).
    pub(super) fn related_iface_cast(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: velt_common::Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if let Some(first) = self.related_dom_union(h.ty, target) {
            let h = self.related_iface_cast(h, first, span)?;
            return self.try_coerce(h, target);
        }
        if !self.related_ifaces(h.ty, target)
            || self.cx.ty.opt_payload(target).is_some()
            || !(self.both_std_dom(h.ty, target) || self.adds_required(h.ty, target))
        {
            return Err(h);
        }
        let Some(any) = self.any_ty() else {
            return Err(h);
        };
        let a = self.compared_operand(h, any);
        Ok(self.intrinsic(hir::Intrinsic::AnyTo, vec![a], target, span))
    }

    /// The first member of `target` when `target` is a union of std `lib.dom` interfaces each
    /// related to `from`'s (`related_ifaces`), and `from` is one too. Only there: converting to
    /// the first member would stop a program whose value is another member, and no `lib.dom`
    /// value exists on a server. Other unions stay `cannot cast` (no run-time test of an `any` for
    /// one of several interfaces yet, #924).
    fn related_dom_union(&mut self, from: TyId, target: TyId) -> Option<TyId> {
        if self.cx.ty.opt_payload(target).is_some() {
            return None;
        }
        let members = self.cx.union_members(target)?;
        let all = members
            .iter()
            .all(|&m| self.related_ifaces(from, m) && self.both_std_dom(from, m));
        match all {
            true => members.first().copied(),
            false => None,
        }
    }

    /// Does interface type `to` declare a required member that `from` lacks (`name()` of `Named`
    /// over `Shape`)? A value lacking one is not a `to` in Node either ([`Self::related_iface_cast`]).
    fn adds_required(&mut self, from: TyId, to: TyId) -> bool {
        let have = self.object_members(from).unwrap_or_default();
        let want = self.object_members(to).unwrap_or_default();
        (want.iter()).any(|m| !m.2 && !have.iter().any(|h| h.0 == m.0))
    }

    /// Are `a` and `b` (or what they hold besides `null`/`undefined`) both of std's `lib.dom`
    /// interfaces?
    fn both_std_dom(&self, a: TyId, b: TyId) -> bool {
        let dom = |t: TyId| {
            let t = self.cx.ty.core(t).unwrap_or(t);
            matches!(self.cx.ty.kind(t), TyKind::Dyn(d, _) if self.cx.std_dom_iface(*d))
        };
        dom(a) && dom(b)
    }
}
