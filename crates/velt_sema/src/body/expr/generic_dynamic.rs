//! Values whose type is known only per instantiation (`T`, `T & object`, `Partial<T>`, a union
//! with a `T` member), used where TypeScript reads their apparent type (issues #1212, #1304,
//! #350): they go through an `any` per instantiation, so a program without such uses is
//! unchanged.

use crate::body::FnCx;
use crate::hir::{self, Intrinsic, LocalId, TyId, TyKind};

impl FnCx<'_, '_> {
    /// Is `t` known only per instantiation: a type parameter, an intersection with one, or a
    /// union or `| null` with such a member (`T`, `T & object`, `T | string`)?
    pub(crate) fn per_instance(&mut self, t: TyId) -> bool {
        fn open(cx: &crate::ctx::Ctx, m: TyId, depth: u32) -> bool {
            match cx.ty.kind(m) {
                TyKind::Param(_) => true,
                TyKind::Op(hir::TyOp::Intersect, parts) if depth < 8 => {
                    parts.iter().any(|&p| open(cx, p, depth + 1))
                }
                // A deferred form over type parameters (`Partial<T>`, `Pick<T, K>`).
                TyKind::Op(hir::TyOp::Inline(_) | hir::TyOp::Alias(_), _) => cx.has_type_params(m),
                _ => false,
            }
        }
        let core = self.cx.ty.opt_payload(t).unwrap_or(t);
        open(self.cx, core, 0)
            || self
                .cx
                .union_members(core)
                .is_some_and(|ms| ms.iter().any(|&m| open(self.cx, m, 0)))
    }

    /// `h` shared into an `any` when its type is known only per instantiation
    /// ([`Self::per_instance`]), for what TypeScript allows on its apparent type that needs the
    /// value's run-time shape (`Object.keys`, `in`, `Array.isArray`, `x[k]`; issue #1212):
    /// lowering boxes each instantiation's value. `Err(h)` for any other `h`.
    pub(crate) fn generic_as_any(&mut self, h: hir::Expr) -> Result<hir::Expr, hir::Expr> {
        if !self.per_instance(h.ty) {
            return Err(h);
        }
        let Some(any) = self.any_ty() else {
            return Err(h);
        };
        let h = self.share_if_place(h);
        self.box_dynamic(h, any)
    }

    /// The variable `e` names when its type is a type parameter or an intersection with one
    /// (not a union or `| null` of it).
    pub(crate) fn own_generic_local(&mut self, e: &velt_syntax::ast::Expr) -> Option<LocalId> {
        use velt_syntax::ast::ExprKind as A;
        let name = match &e.kind {
            A::Paren(x) => return self.own_generic_local(x),
            A::Ident(i) => i.name.as_str(),
            _ => return None,
        };
        let l = self
            .f
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).copied())?;
        let t = self.local_ty(l);
        let plain = self.cx.ty.opt_payload(t).is_none() && self.cx.union_members(t).is_none();
        (plain && self.per_instance(t)).then_some(l)
    }

    /// `x as D` where `x`'s type or `D` is known only per instantiation and nothing else
    /// converts it (`objectTarget as Signal<T & object>`, which asserts members the value may
    /// lack; issue #1304): the value through an `any`, checked at run time as `AnyTo` checks
    /// it, so an instantiation where the value already is a `D` keeps it, and any other stops
    /// with a `TypeError` naming both types.
    pub(crate) fn generic_assertion(
        &mut self,
        h: hir::Expr,
        target: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        // A primitive cast to a key type known only per instantiation (`key as SignalKey<T>`
        // with `key: string`) is checked the same way: the string must be one of the keys.
        let primitive = matches!(
            self.cx.ty.kind(h.ty),
            TyKind::Str | TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool
        );
        if !((self.per_instance(h.ty) || primitive) && self.per_instance(target)) {
            return Err(h);
        }
        let span = h.span;
        let boxed = match primitive {
            true => {
                let Some(any) = self.any_ty() else {
                    return Err(h);
                };
                let h = self.share_if_place(h);
                self.box_dynamic(h, any)?
            }
            false => self.generic_as_any(h)?,
        };
        let boxed = self.share_if_place(boxed);
        Ok(self.intrinsic(Intrinsic::AnyTo, vec![boxed], target, span))
    }

    /// A key known only per instantiation (a value of `K extends SignalKey<T>`, of `keyof T`
    /// or another deferred form of string keys) where a `string` is expected: the key's
    /// string, through an `any`. Only for a form that gives strings at the type parameters'
    /// bounds. `Err(h)` for any other value.
    pub(crate) fn generic_key_string(
        &mut self,
        h: hir::Expr,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        if exp != self.cx.ty.str_ {
            return Err(h);
        }
        let form = match self.cx.ty.kind(h.ty) {
            TyKind::Param(p) => match self.cx.constraint_of(*p) {
                Some(c) => c,
                None => return Err(h),
            },
            _ => h.ty,
        };
        if !matches!(
            self.cx.ty.kind(form),
            TyKind::Op(hir::TyOp::Alias(_) | hir::TyOp::Inline(_), _)
        ) {
            return Err(h);
        }
        // The form at the type parameters' object bounds (or `{}`): string keys?
        let n = self.bounds.len() as u32;
        let at_bounds: Vec<TyId> = (0..n)
            .map(|p| {
                self.cx
                    .constraint_of(p)
                    .filter(|&c| !self.cx.has_type_params(c))
                    .unwrap_or_else(|| self.cx.ty.param(p))
            })
            .collect();
        let at = self.cx.subst(form, &at_bounds);
        let str_ = self.cx.ty.str_;
        let strings = |cx: &mut crate::ctx::Ctx, t: TyId| {
            let ms = cx.union_members(t).unwrap_or_else(|| vec![t]);
            ms.iter().all(|&m| {
                m == str_ || matches!(cx.ty.kind(m), TyKind::Literal(hir::LitValue::Str(_)))
            })
        };
        if !strings(self.cx, at) && self.cx.has_type_params(at) {
            // Still deferred at the bounds (`keyof T` of an unbounded `T`): its keys are strings
            // whenever it is used as a key here.
        } else if !strings(self.cx, at) {
            return Err(h);
        }
        let Some(any) = self.any_ty() else {
            return Err(h);
        };
        let span = h.span;
        let h = self.share_if_place(h);
        let boxed = self.box_dynamic(h, any)?;
        Ok(self.intrinsic(Intrinsic::AnyTo, vec![boxed], str_, span))
    }
}
