//! A value of a type parameter passed for a parameter whose union has that type parameter as a
//! member, in generic code (`g<T>(v)` for `g<T>(x: T | Foo)`, also `T | null`, `T |
//! undefined` and an option of such a union; #1456). At `T = any` / `unknown` (or `object`)
//! the value may be one of the other members' (a `Foo`, `null`, `true`, `"a"`), while the
//! tests on the union (`instanceof`, `===`) look at the member the value sits in. So the
//! conversion asks the value, as converting an `any` to the union does at a call site that is
//! not generic (`dynamic_to_members`):
//!
//! ```text
//! HoldsDynamic(<T>) ? (test_1(v as any) ? M_1(v as any as M_1) : … : T(v as any as T)) : T(v)
//! ```
//!
//! The condition is fixed per instantiation and lowering takes only its branch, so the other
//! instantiations lower the plain wrap, the code they had without it. The routed branch reads
//! the value once per test and converts it once in each arm, so it declares no temporary (a
//! local would be declared at every instantiation) and moves the value where the plain wrap
//! does. Only a variable or field path is routed: reading it again has no effect.

use velt_common::Span;

use crate::body::places::{is_place, set_place_mode};
use crate::body::FnCx;
use crate::hir::{self, DefId, ExprKind as H, Intrinsic, Nothing, TyId, TyKind, UseMode};

/// The union a value of a type parameter is wrapped into: its def, type arguments and the
/// parameter's variant.
type Member = (DefId, Vec<TyId>, u32);

impl FnCx<'_, '_> {
    /// Argument `h`, converted for its parameter, routed by its value at a dynamic
    /// instantiation when it wraps a value of a type parameter into a union member or an
    /// option (module docs); `h` itself otherwise.
    pub(super) fn routed_param_arg(&mut self, h: hir::Expr) -> hir::Expr {
        if self.cx.ty.nothing_of(h.ty) == Some(Nothing::Both) {
            return self.routed_both_arg(h);
        }
        let (value, member) = match &h.kind {
            H::Variant {
                def,
                type_args,
                variant,
                args,
            } if args.len() == 1 => (&args[0], Some((*def, type_args.clone(), *variant))),
            H::WrapSome(inner) => match &inner.kind {
                H::Variant {
                    def,
                    type_args,
                    variant,
                    args,
                } if args.len() == 1 => (&args[0], Some((*def, type_args.clone(), *variant))),
                _ => (&**inner, None),
            },
            _ => return h,
        };
        if !matches!(self.cx.ty.kind(value.ty), TyKind::Param(_))
            || !is_place(value)
            || self.cx.scopes[self.module].is_std
            || self.any_ty().is_none()
        {
            return h;
        }
        let (span, ty) = (h.span, h.ty);
        let wrapped = matches!(h.kind, H::WrapSome(_));
        let routes = match &member {
            Some((_, _, own)) if !wrapped => {
                let members = self.cx.union_members(ty).unwrap_or_default();
                !self.tested_members(&members, *own).is_empty()
            }
            _ => matches!(
                self.cx.ty.nothing_of(ty),
                Some(Nothing::Null | Nothing::Undefined)
            ),
        };
        if !routes {
            return h;
        }
        let value = value.clone();
        let routed = match member {
            Some(m) if !wrapped => self.route_to_members(&value, ty, m, span),
            member => self.route_to_option(&value, ty, member, span),
        };
        let Some(then) = routed else {
            return h;
        };
        let bool_ = self.cx.ty.bool_;
        let probe = self.intrinsic(Intrinsic::NullAs, vec![], value.ty, span);
        let cond = self.intrinsic(Intrinsic::HoldsDynamic, vec![probe], bool_, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(h),
        };
        self.mk(kind, ty, span)
    }

    /// [`Self::routed_param_arg`] for a `T | null | undefined` parameter, whose present value
    /// is `WrapSome(WrapSome(v))` (hir_encodings.md "Undefined"): `undefined` and `null` go to
    /// their own nothings.
    fn routed_both_arg(&mut self, h: hir::Expr) -> hir::Expr {
        let H::WrapSome(outer) = &h.kind else {
            return h;
        };
        let H::WrapSome(inner) = &outer.kind else {
            return h;
        };
        let (value, member) = match &inner.kind {
            H::Variant {
                def,
                type_args,
                variant,
                args,
            } if args.len() == 1 => (&args[0], Some((*def, type_args.clone(), *variant))),
            _ => (&**inner, None),
        };
        if !matches!(self.cx.ty.kind(value.ty), TyKind::Param(_))
            || !is_place(value)
            || self.cx.scopes[self.module].is_std
            || self.any_ty().is_none()
        {
            return h;
        }
        let (exp, span) = (h.ty, h.span);
        let Some(payload_ty) = self.cx.ty.opt_payload(exp) else {
            return h;
        };
        let value = value.clone();
        let present = match member {
            Some(m) => {
                let Some(union) = self.cx.ty.opt_payload(payload_ty) else {
                    return h;
                };
                match self.route_to_members(&value, union, m.clone(), span) {
                    Some(routed) => routed,
                    None => {
                        let back = self.any_back(&value, span);
                        let (def, type_args, variant) = m;
                        let kind = H::Variant {
                            def,
                            type_args,
                            variant,
                            args: vec![back],
                        };
                        self.mk(kind, union, span)
                    }
                }
            }
            None => self.any_back(&value, span),
        };
        let some = self.mk(H::WrapSome(Box::new(present)), payload_ty, span);
        let some = self.mk(H::WrapSome(Box::new(some)), exp, span);
        let null = self.mk(H::Lit(hir::Lit::Null), payload_ty, span);
        let null = self.mk(H::WrapSome(Box::new(null)), exp, span);
        let null = self.taken_over(&value, null);
        let undefined = self.mk(H::Lit(hir::Lit::Null), exp, span);
        let undefined = self.taken_over(&value, undefined);
        let (Some(is_null), Some(is_undefined)) = (
            self.nothing_test(&value, Nothing::Null, span),
            self.nothing_test(&value, Nothing::Undefined, span),
        ) else {
            return h;
        };
        let rest = self.mk(
            H::If {
                cond: Box::new(is_null),
                then: Box::new(null),
                els: Box::new(some),
            },
            exp,
            span,
        );
        let then = self.mk(
            H::If {
                cond: Box::new(is_undefined),
                then: Box::new(undefined),
                els: Box::new(rest),
            },
            exp,
            span,
        );
        let bool_ = self.cx.ty.bool_;
        let probe = self.intrinsic(Intrinsic::NullAs, vec![], value.ty, span);
        let cond = self.intrinsic(Intrinsic::HoldsDynamic, vec![probe], bool_, span);
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(h),
        };
        self.mk(kind, exp, span)
    }

    /// Is `value` (of a type parameter, read as an `any`) the nothing `n` (`null` or
    /// `undefined`)?
    fn nothing_test(&mut self, value: &hir::Expr, n: Nothing, span: Span) -> Option<hir::Expr> {
        let (bool_, any) = (self.cx.ty.bool_, self.any_ty()?);
        let x = self.read_as_any(value);
        Some(match n {
            Nothing::Null => {
                let null = self.dynamic_nothing(Nothing::Null, any, span);
                self.intrinsic(Intrinsic::AnyStrictEq, vec![x, null], bool_, span)
            }
            Nothing::Undefined => {
                let str_ = self.cx.ty.str_;
                let tag = self.intrinsic(Intrinsic::AnyTypeof, vec![x], str_, span);
                let want = self.str_lit("undefined", span);
                let kind = H::Binary {
                    op: hir::BinOp::Eq,
                    lhs: Box::new(tag),
                    rhs: Box::new(want),
                };
                self.mk(kind, bool_, span)
            }
            Nothing::Both => return None,
        })
    }

    /// `value` read as an `any` for a test (a share of it).
    fn read_as_any(&mut self, value: &hir::Expr) -> hir::Expr {
        let mut v = value.clone();
        set_place_mode(&mut v, UseMode::Borrow);
        self.param_as_any(v)
    }

    /// `value` as an `any` to convert: taken over where the plain wrap takes it, else shared.
    fn owned_as_any(&mut self, value: &hir::Expr) -> hir::Expr {
        match (moved(value), self.any_ty()) {
            (true, Some(any)) => self.coerce(value.clone(), any),
            _ => self.param_as_any(value.clone()),
        }
    }

    /// `value` (of a type parameter) as union `exp` (`target`), by the first other member
    /// whose test the value passes; `None` when no other member has a test.
    fn route_to_members(
        &mut self,
        value: &hir::Expr,
        exp: TyId,
        target: Member,
        span: Span,
    ) -> Option<hir::Expr> {
        let (def, type_args, own) = target;
        let members = self.cx.union_members(exp)?;
        let tested = self.tested_members(&members, own);
        if tested.is_empty() {
            return None;
        }
        let variant = |s: &mut Self, v: u32, payload: hir::Expr| {
            let kind = H::Variant {
                def,
                type_args: type_args.clone(),
                variant: v,
                args: vec![payload],
            };
            s.mk(kind, exp, span)
        };
        let back = self.any_back(value, span);
        let mut result = variant(self, own, back);
        for &v in tested.iter().rev() {
            let x = self.read_as_any(value);
            let owned = self.owned_as_any(value);
            let m = members[v as usize];
            let Some((test, payload)) = self.member_test_on(x, owned, m, span) else {
                continue;
            };
            // A literal member's value is a constant: the arm takes the value over all the same.
            let payload = match self.cx.lit_value(m) {
                Some(_) => self.taken_over(value, payload),
                None => payload,
            };
            let then = variant(self, v, payload);
            let kind = H::If {
                cond: Box::new(test),
                then: Box::new(then),
                els: Box::new(result),
            };
            result = self.mk(kind, exp, span);
        }
        Some(result)
    }

    /// The members of a union other than the parameter's (`own`) that `member_test_on` tells
    /// apart (a primitive, a literal, a class, the one array), by variant index.
    fn tested_members(&self, members: &[TyId], own: u32) -> Vec<u32> {
        let is_array = |m: TyId| matches!(self.cx.ty.kind(m), TyKind::Array(_));
        let arrays = members.iter().filter(|m| is_array(**m)).count();
        let tested = |m: TyId| match self.cx.ty.kind(m) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Str | TyKind::Bool | TyKind::Symbol => true,
            TyKind::Array(_) => arrays == 1 && !self.uses_proxy(),
            _ => self.cx.lit_value(m).is_some() || self.cx.class_of(m).is_some(),
        };
        (0..members.len() as u32)
            .filter(|&v| v != own && tested(members[v as usize]))
            .collect()
    }

    /// `value` (of a type parameter) as `exp`, a `T | null` or `T | undefined`, or an option
    /// of a union with `T` as `member` (routed by the other members' tests too): the nothing
    /// when the value is that nothing.
    fn route_to_option(
        &mut self,
        value: &hir::Expr,
        exp: TyId,
        member: Option<Member>,
        span: Span,
    ) -> Option<hir::Expr> {
        let nothing = self.cx.ty.nothing_of(exp)?;
        let test = self.nothing_test(value, nothing, span)?;
        let present = match member {
            Some(m) => {
                let union = self.cx.ty.opt_payload(exp)?;
                match self.route_to_members(value, union, m.clone(), span) {
                    Some(routed) => routed,
                    None => {
                        let back = self.any_back(value, span);
                        let (def, type_args, variant) = m;
                        let kind = H::Variant {
                            def,
                            type_args,
                            variant,
                            args: vec![back],
                        };
                        self.mk(kind, union, span)
                    }
                }
            }
            None => self.any_back(value, span),
        };
        let none = self.mk(H::Lit(hir::Lit::Null), exp, span);
        let none = self.taken_over(value, none);
        let some = self.mk(H::WrapSome(Box::new(present)), exp, span);
        let kind = H::If {
            cond: Box::new(test),
            then: Box::new(none),
            els: Box::new(some),
        };
        Some(self.mk(kind, exp, span))
    }

    /// `value` (of a type parameter), passed through an `any`, as its own type again: the
    /// member it stays in when no other member's test passes.
    fn any_back(&mut self, value: &hir::Expr, span: Span) -> hir::Expr {
        let owned = self.owned_as_any(value);
        self.intrinsic(Intrinsic::AnyTo, vec![owned], value.ty, span)
    }

    /// `e`, after taking over `value` where the plain wrap takes it (`{ value; e }`), so every
    /// branch moves it as the wrap does: a value moved on one path only would need a drop flag
    /// at every instantiation.
    fn taken_over(&mut self, value: &hir::Expr, e: hir::Expr) -> hir::Expr {
        if !moved(value) {
            return e;
        }
        let (ty, span) = (e.ty, e.span);
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Expr(value.clone()),
                span,
            }],
            value: Some(Box::new(e)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }
}

/// Is `value` (a place) moved out of by the plain wrap?
fn moved(value: &hir::Expr) -> bool {
    matches!(
        value.kind,
        H::Local(_, UseMode::Move)
            | H::Field {
                mode: UseMode::Move,
                ..
            }
            | H::UnwrapSome(_, UseMode::Move)
            | H::UnwrapVariant {
                mode: UseMode::Move,
                ..
            }
    )
}
