//! Type assertions that narrow (docs/reference/types.md "Type assertions"): `x as T` with
//! `x: T | null` (or `| undefined`), and `u as A` with `u` a union that has the member `A`.
//! TypeScript trusts them; Velt checks them, as it checks `x!`: the value is `T`'s, or the
//! program panics (`type assertion failed: …`). A value of the wrong type never reads as `T`.
//!
//! Not in generic code: at `T = U | null`, `T | null` is `U | null`, so a value that is `null`
//! is a `T` there, and the check would reject it.

use velt_common::Span;

use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, ExprKind as H, Intrinsic, PatKind as P, TyId, UseMode};
use crate::types::collect_params;

use super::union_coerce::outer_mode;

impl FnCx<'_, '_> {
    /// `h as target` when `target` is narrower than `h`'s type in one of the ways above, checked
    /// at run time; `Err(h)` otherwise.
    pub(super) fn narrowing_assertion(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let generic = |s: &Self, t: TyId| {
            let mut ps = vec![];
            collect_params(&s.cx.ty, t, &mut ps);
            !ps.is_empty()
        };
        if generic(self, h.ty) || generic(self, target) || self.cx.ty.has_error(h.ty) {
            return Err(h);
        }
        let t = &self.cx.ty;
        if t.nothing_of(target).is_some() {
            return Err(h);
        }
        match (t.nothing_of(h.ty), t.core(h.ty)) {
            (Some(n), Some(core)) => {
                if core != target && !self.has_member(core, target) {
                    return Err(h);
                }
                let msg = format!("type assertion failed: the value is {}", n.word());
                let present = self.checked_present(h, &msg, None, false, span);
                if present.ty == target {
                    return Ok(present);
                }
                self.member_assertion(present, target, span)
            }
            _ if self.has_member(h.ty, target) => self.member_assertion(h, target, span),
            _ => Err(h),
        }
    }

    /// Is `target` a member of union `u`?
    fn has_member(&mut self, u: TyId, target: TyId) -> bool {
        self.cx
            .union_members(u)
            .is_some_and(|ms| ms.contains(&target))
    }

    /// Union value `h` as its member `target`: `match (h) { Target(v) => v, _ => panic }`.
    fn member_assertion(
        &mut self,
        h: hir::Expr,
        target: TyId,
        span: Span,
    ) -> Result<hir::Expr, hir::Expr> {
        let (Some((def, _)), Some(members)) =
            (self.cx.union_def(h.ty), self.cx.union_members(h.ty))
        else {
            return Err(h);
        };
        let Some(v) = members.iter().position(|m| *m == target) else {
            return Err(h);
        };
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let (sub, value) = if self.cx.lit_value(target).is_some() {
            let wild = hir::Pat {
                kind: P::Wildcard,
                ty: target,
                span,
            };
            (wild, self.lit_const(target, span))
        } else {
            let mode = if self.cx.is_copy(target) {
                UseMode::Copy
            } else if consume {
                UseMode::Move
            } else {
                UseMode::Borrow
            };
            let b = self.new_local("<asserted>", target, false, span, LocalKind::Bind);
            let bind = hir::Pat {
                kind: P::Binding(b, mode),
                ty: target,
                span,
            };
            (bind, self.mk(H::Local(b, mode), target, span))
        };
        let shown = self.cx.display(target);
        let msg = self.str_lit(
            &format!("type assertion failed: the value is not of type `{shown}`"),
            span,
        );
        let never = self.cx.ty.never;
        let arms = vec![
            hir::Arm {
                pat: hir::Pat {
                    kind: P::Variant {
                        def,
                        variant: v as u32,
                        args: vec![sub],
                    },
                    ty: h.ty,
                    span,
                },
                guard: None,
                body: value,
            },
            hir::Arm {
                pat: hir::Pat {
                    kind: P::Wildcard,
                    ty: h.ty,
                    span,
                },
                guard: None,
                body: self.intrinsic(Intrinsic::Panic, vec![msg], never, span),
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(kind, target, span))
    }
}
