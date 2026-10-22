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
        // `x as Record<string, any>` of a union with a record viewed with named members
        // (`crate::index_signatures`): the assertion to that member, as its record.
        let core = self.cx.ty.core(h.ty).unwrap_or(h.ty);
        let view = self.cx.union_members(core).and_then(|ms| {
            ms.into_iter().find(|m| {
                self.cx
                    .record_view(*m)
                    .is_some_and(|(base, _)| base == target)
            })
        });
        if let Some(view) = view {
            return self
                .narrowing_assertion(h, view, span)
                .map(|h| self.unview(h));
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
                let msg = format!(
                    "TypeError: type assertion failed: the value is {}",
                    n.word()
                );
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

    /// `x as T & object` for an `x` whose type is the type parameter `T` (or `T | null`)
    /// without an `object` bound: sigx's `target as T & object` after a primitive test.
    /// `T & object` is `T` at an object type and `never` at a primitive, so the value is
    /// first `x as object`, checked at run time ([`param_as_object`](Self::param_as_object):
    /// a primitive is a `TypeError`), then that object as `T & object`. The value is shared,
    /// not copied. `Err(h)` for other casts.
    pub(super) fn param_as_object_part(
        &mut self,
        h: hir::Expr,
        target: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        use crate::any::Dynamic;
        let inner = self.cx.ty.opt_payload(h.ty).unwrap_or(h.ty);
        let hir::TyKind::Op(hir::TyOp::Intersect, parts) = self.cx.ty.kind(target).clone() else {
            return Err(h);
        };
        let mut objects = None;
        let mut rest = vec![];
        for p in parts {
            match self.cx.ty.kind(p) {
                hir::TyKind::Op(hir::TyOp::Intersect, ps) => rest.extend(ps.iter().copied()),
                _ => rest.push(p),
            }
        }
        rest.retain(|&p| match self.cx.dynamic(p) {
            Some(Dynamic::Object) => {
                objects = Some(p);
                false
            }
            _ => true,
        });
        let Some(object) = objects else {
            return Err(h);
        };
        if !matches!(self.cx.ty.kind(inner), hir::TyKind::Param(_))
            || rest.iter().any(|&p| p != inner)
        {
            return Err(h);
        }
        let boxed = self.param_as_object(h, object)?;
        self.coerce_dynamic(boxed, target, true)
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
            &format!("TypeError: type assertion failed: the value is not of type `{shown}`"),
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
