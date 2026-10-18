//! Nothings held by a union member (#1268). `T | string | undefined` at `T = number | null`
//! keeps the generic union's layout (velt_sema `anon::canon_union`, velt_vir `canon_union`):
//! its `null` is the `T` member holding `null`, while the same type written out,
//! `number | string | null | undefined`, is one option of a plain union. A value converts
//! between the two forms state by state: each nothing and each member of the source becomes
//! the target's.
//!
//! Outside generic code, a value of such a type (a generic function's result, a field of a
//! generic class) is converted to the written-out form where it is read
//! (`flatten_hidden_nothings`), so `== null`, `??` and narrowing see every nothing it holds.

use crate::body::places::{is_place, set_place_mode};
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, Nothing, PatKind as P, TyId, UseMode};

impl FnCx<'_, '_> {
    /// The nothings `t` holds through members of its union (`None`: none).
    pub(super) fn hidden_nothing(&mut self, t: TyId) -> Option<Nothing> {
        let core = self.cx.ty.core(t).unwrap_or(t);
        self.cx.union_def(core)?;
        let members = self.cx.union_members(core)?;
        members
            .iter()
            .filter_map(|m| self.cx.ty.nothing_of(*m))
            .reduce(Nothing::join)
    }

    /// Every nothing `t` holds: its own and its members'.
    pub(super) fn all_nothings(&mut self, t: TyId) -> Option<Nothing> {
        match (self.cx.ty.nothing_of(t), self.hidden_nothing(t)) {
            (Some(a), Some(b)) => Some(a.join(b)),
            (a, b) => a.or(b),
        }
    }

    /// Does a value of `from` convert to `exp` state by state (`coerce_hidden_nothings`):
    /// either holds a nothing through a union member, and `from` has states to match on.
    pub(super) fn converts_by_state(&mut self, from: TyId, exp: TyId) -> bool {
        if self.hidden_nothing(from).is_none() && self.hidden_nothing(exp).is_none() {
            return false;
        }
        let core = self.cx.ty.core(from).unwrap_or(from);
        self.cx.ty.nothing_of(from).is_some() || self.cx.union_def(core).is_some()
    }

    /// `h` converted to `exp` state by state (`converts_by_state` holds): `Err` when a
    /// nothing or a member of `h` has no place in `exp`.
    pub(super) fn coerce_hidden_nothings(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let found = self.all_nothings(h.ty);
        let want = self.all_nothings(exp);
        if let Some(f) = found {
            if !want.is_some_and(|w| f.within(w)) {
                return Err(h);
            }
        }
        self.by_state(h, exp)
    }

    fn by_state(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let sty = h.ty;
        let span = h.span;
        let consume = !is_place(&h) || super::union_coerce::outer_mode(&h) == Some(UseMode::Move);
        let mut arms = vec![];
        for n in self.nothing_arms(sty) {
            let pat = self.one_nothing_pat(n, sty, span);
            let Some(body) = self.nothing_as(n, exp, span) else {
                return Err(h);
            };
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        let core = self.cx.ty.core(sty).unwrap_or(sty);
        let parts: Vec<(Option<u32>, TyId)> = match self.cx.union_def(core) {
            Some(_) => self
                .cx
                .union_members(core)
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .map(|(v, m)| (Some(v as u32), m))
                .collect(),
            None => vec![(None, core)],
        };
        let def = self.cx.union_def(core).map(|(d, _)| d);
        for (v, m) in parts {
            let (sub, value) = self.member_binding(m, consume, span);
            let Ok(body) = self.try_coerce(value, exp) else {
                return Err(h);
            };
            let mut pat = match (def, v) {
                (Some(def), Some(variant)) => self.pat(
                    P::Variant {
                        def,
                        variant,
                        args: vec![sub],
                    },
                    core,
                    span,
                ),
                _ => sub,
            };
            if core != sty {
                pat = self.present_pat(pat, sty, span);
            }
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(kind, exp, span))
    }

    /// Nothing `which` as a value of `exp`: its own, or a member's (`nothing_in_member`).
    fn nothing_as(
        &mut self,
        which: Nothing,
        exp: TyId,
        span: velt_common::Span,
    ) -> Option<hir::Expr> {
        let own = self.cx.ty.nothing_of(exp).is_some_and(|n| match which {
            Nothing::Undefined => n.has_undefined(),
            _ => n.has_null(),
        });
        if own {
            return Some(self.nothing_value(which, exp, span));
        }
        self.nothing_in_member(which, exp, span)
    }

    /// `h` in the written-out form of its type when that holds a nothing through a union member
    /// (module docs): `T | string | undefined` at `T = number | null` becomes
    /// `number | string | null | undefined`. A place is read as a share.
    pub(crate) fn flatten_hidden_nothings(&mut self, mut h: hir::Expr) -> hir::Expr {
        if self.hidden_nothing(h.ty).is_none() || self.cx.has_type_params(h.ty) {
            return h;
        }
        let core = self.cx.ty.core(h.ty).unwrap_or(h.ty);
        let members = self.cx.union_members(core).unwrap_or_default();
        let own = self.cx.ty.nothing_of(h.ty);
        let flat = self.cx.union_with(&members, own, h.span);
        if flat == h.ty || self.cx.ty.has_error(flat) {
            return h;
        }
        if is_place(&h) {
            set_place_mode(&mut h, UseMode::Borrow);
        }
        match self.coerce_hidden_nothings(h, flat) {
            Ok(h) | Err(h) => h,
        }
    }
}
