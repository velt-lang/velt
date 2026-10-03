//! Implicit conversions into union types (hir_encodings.md "Union types"):
//! - a member value is wrapped in its variant (`ExprKind::Variant`); a subclass converts to a
//!   base-class member and a concrete value to an interface member like elsewhere;
//! - a union value converts to a union with at least its members by re-tagging
//!   (`match (x) { A(a) => Wider.A(a), ... }`); a local narrowed to some of its members only
//!   needs those (the other variants are unreachable).
//!
//! The converted value keeps the use mode of its expression, like `WrapSome`: a borrowed place
//! wrapped for a borrowed parameter is copied by lowering, a moved one is moved.

use velt_common::Span;

use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, DefId, ExprKind as H, Intrinsic, PatKind as P, TyId, UseMode};

impl FnCx<'_, '_> {
    /// `h` converted into union type `exp` (`Err` gives it back).
    pub(super) fn coerce_to_union(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some((def, args)) = self.cx.union_def(exp) else {
            return Err(h);
        };
        let targets = self.cx.union_members(exp).unwrap_or_default();
        if self.cx.union_def(h.ty).is_some() {
            return self.retag(h, exp, &targets);
        }
        if let Some(v) = targets.iter().position(|t| *t == h.ty) {
            return Ok(self.union_variant(def, &args, v as u32, h, exp));
        }
        let mut h = h;
        for (v, t) in targets.iter().enumerate() {
            match self.try_coerce(h, *t) {
                Ok(c) => return Ok(self.union_variant(def, &args, v as u32, c, exp)),
                Err(back) => h = back,
            }
        }
        Err(h)
    }

    fn union_variant(
        &self,
        def: DefId,
        type_args: &[TyId],
        variant: u32,
        h: hir::Expr,
        ty: TyId,
    ) -> hir::Expr {
        let span = h.span;
        let kind = H::Variant {
            def,
            type_args: type_args.to_vec(),
            variant,
            args: vec![h],
        };
        self.mk(kind, ty, span)
    }

    /// Union value `h` → union `exp` (`targets`): every member `h` can hold is a member of
    /// `exp` or converts to one (a literal to its base, a subclass to its base class, ...).
    fn retag(&mut self, h: hir::Expr, exp: TyId, targets: &[TyId]) -> Result<hir::Expr, hir::Expr> {
        let Some((tdef, targs)) = self.cx.union_def(exp) else {
            return Err(h);
        };
        self.map_members(h, exp, &mut |s, value| {
            if let Some(j) = targets.iter().position(|t| *t == value.ty) {
                return Some(s.union_variant(tdef, &targs, j as u32, value, exp));
            }
            let mut value = value;
            for (j, t) in targets.iter().enumerate() {
                match s.try_coerce(value, *t) {
                    Ok(c) => return Some(s.union_variant(tdef, &targs, j as u32, c, exp)),
                    Err(back) => value = back,
                }
            }
            None
        })
    }

    /// Union value `h` → non-union `exp` that every member it can hold converts to (a union of
    /// literals to their base type, subclasses to a base class, implementors to an interface).
    pub(super) fn union_to_common(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        self.map_members(h, exp, &mut |s, value| s.try_coerce(value, exp).ok())
    }

    /// `match (h) { V(m) => f(m), ... }` over the variants union value `h` can hold (narrowing
    /// included); `Err(h)` when `f` rejects a member. Literal members are matched without a
    /// binding and passed as their constant.
    fn map_members(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        f: &mut dyn FnMut(&mut Self, hir::Expr) -> Option<hir::Expr>,
    ) -> Result<hir::Expr, hir::Expr> {
        let Some((sdef, _)) = self.cx.union_def(h.ty) else {
            return Err(h);
        };
        let src = self.cx.union_members(h.ty).unwrap_or_default();
        let live = self
            .narrowed_variants(&h)
            .unwrap_or_else(|| (0..src.len() as u32).collect());
        let span = h.span;
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let mut arms = vec![];
        for v in &live {
            let m = src[*v as usize];
            let (sub, value) = self.member_binding(m, consume, span);
            let Some(body) = f(self, value) else {
                return Err(h);
            };
            let pat = hir::Pat {
                kind: P::Variant {
                    def: sdef,
                    variant: *v,
                    args: vec![sub],
                },
                ty: h.ty,
                span,
            };
            arms.push(hir::Arm {
                pat,
                guard: None,
                body,
            });
        }
        if live.len() < src.len() {
            arms.push(self.unreachable_arm(h.ty, span));
        }
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(kind, exp, span))
    }

    /// The payload pattern and value of a union member of type `m` in a re-tagging arm.
    fn member_binding(&mut self, m: TyId, consume: bool, span: Span) -> (hir::Pat, hir::Expr) {
        if self.cx.lit_value(m).is_some() {
            let wild = hir::Pat {
                kind: P::Wildcard,
                ty: m,
                span,
            };
            return (wild, self.lit_const(m, span));
        }
        let mode = if self.cx.is_copy(m) {
            UseMode::Copy
        } else if consume {
            UseMode::Move
        } else {
            UseMode::Borrow
        };
        let b = self.new_local("<union>", m, false, span, LocalKind::Bind);
        let bind = hir::Pat {
            kind: P::Binding(b, mode),
            ty: m,
            span,
        };
        (bind, self.mk(H::Local(b, mode), m, span))
    }

    /// `T | null` → `U | null` for a type `U` that `T` converts to (a union, an interface, a
    /// base class, ...): `match (h) { Some(v) => Some(<v as U>), null => null }`.
    pub(super) fn option_to_option(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let (Some(hp), Some(ep)) = (self.cx.ty.opt_payload(h.ty), self.cx.ty.opt_payload(exp))
        else {
            return Err(h);
        };
        let span = h.span;
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let mode = if self.cx.is_copy(hp) {
            UseMode::Copy
        } else if consume {
            UseMode::Move
        } else {
            UseMode::Borrow
        };
        let b = self.new_local("<some>", hp, false, span, LocalKind::Bind);
        let value = self.mk(H::Local(b, mode), hp, span);
        let Ok(converted) = self.try_coerce(value, ep) else {
            return Err(h);
        };
        let bind = hir::Pat {
            kind: P::Binding(b, mode),
            ty: hp,
            span,
        };
        let arms = vec![
            hir::Arm {
                pat: hir::Pat {
                    kind: P::Some(Box::new(bind)),
                    ty: h.ty,
                    span,
                },
                guard: None,
                body: self.mk(H::WrapSome(Box::new(converted)), exp, span),
            },
            hir::Arm {
                pat: hir::Pat {
                    kind: P::None,
                    ty: h.ty,
                    span,
                },
                guard: None,
                body: self.mk(H::Lit(hir::Lit::Null), exp, span),
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(kind, exp, span))
    }

    /// `_ => panic(...)` for variants flow narrowing ruled out.
    pub(super) fn unreachable_arm(&self, ty: TyId, span: Span) -> hir::Arm {
        let msg = self.str_lit("unreachable union member", span);
        let never = self.cx.ty.never;
        hir::Arm {
            pat: hir::Pat {
                kind: P::Wildcard,
                ty,
                span,
            },
            guard: None,
            body: self.intrinsic(Intrinsic::Panic, vec![msg], never, span),
        }
    }

    /// The variants a (narrowed) union local read `h` can hold, if narrowed.
    pub(super) fn narrowed_variants(&self, h: &hir::Expr) -> Option<Vec<u32>> {
        match &h.kind {
            H::Local(l, _) => self.allowed_members(*l),
            H::UnwrapSome(base, _) => self.narrowed_variants(base),
            _ => None,
        }
    }
}

fn outer_mode(e: &hir::Expr) -> Option<UseMode> {
    match e.kind {
        H::Local(_, m)
        | H::Field { mode: m, .. }
        | H::Index { mode: m, .. }
        | H::UnwrapSome(_, m)
        | H::UnwrapVariant { mode: m, .. } => Some(m),
        H::Downcast(ref x) => outer_mode(x),
        _ => None,
    }
}
