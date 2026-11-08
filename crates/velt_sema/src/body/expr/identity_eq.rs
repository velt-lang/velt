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
        if l.ty == r.ty || !object(self, l.ty) || !object(self, r.ty) {
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
    /// extending the other (`Node` and `Comment`)? Such values don't convert to each other yet
    /// (`iface_notes`), so `===` compares them as `any` values: the object each points at.
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

    /// `e as B` for an `A` value where `B` extends `A`, both of std's `lib.dom` interfaces
    /// (`node as HTMLElement`), which `tsc` allows since one is assignable to the other: the value
    /// as an `any`, converted to `B` (`Err`: not such a pair). No such value exists on a server, so
    /// the conversion never runs; between other interfaces it is still `cannot cast`, since the
    /// check would stop a program where Node goes on.
    pub(super) fn related_iface_cast(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: velt_common::Span,
    ) -> Result<hir::Expr, hir::Expr> {
        if !self.related_ifaces(h.ty, target)
            || self.cx.ty.opt_payload(target).is_some()
            || !self.both_std_dom(h.ty, target)
        {
            return Err(h);
        }
        let Some(any) = self.any_ty() else {
            return Err(h);
        };
        let a = self.compared_operand(h, any);
        Ok(self.intrinsic(hir::Intrinsic::AnyTo, vec![a], target, span))
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
