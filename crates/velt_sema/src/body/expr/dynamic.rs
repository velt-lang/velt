//! Conversions to and from `any` / `unknown` (crate::any; hir_encodings.md "any and
//! unknown"). Every value converts to either (`ToDyn` with `Program::any_impl`, after
//! unpacking `T | null`, unions and literal types, so the box holds one concrete type). An
//! `any` converts to every type, checked at run time (`Intrinsic::AnyTo`); an `unknown` only
//! to `any`, or with `as T`.

use velt_common::{Diagnostic, Span};

use super::union_coerce::outer_mode;
use crate::any::Dynamic;
use crate::body::places::is_place;
use crate::body::{FnCx, LocalKind};
use crate::hir::{
    self, ExprKind as H, Intrinsic, LocalId, Nothing, PatKind as P, TyId, TyKind, UseMode,
};

impl FnCx<'_, '_> {
    /// `h` converted to `exp`, one of them `any` or `unknown`. `checked_from_unknown`: an
    /// `unknown` may convert to a typed value (`as T`).
    pub(crate) fn coerce_dynamic(
        &mut self,
        h: hir::Expr,
        exp: TyId,
        checked_from_unknown: bool,
    ) -> Result<hir::Expr, hir::Expr> {
        let to = self.cx.dynamic(exp);
        let from = self.cx.dynamic(h.ty);
        match (from, to) {
            (None, None) => Err(h),
            (None, Some(_)) => self.box_dynamic(h, exp),
            (Some(Dynamic::Unknown), None) if !checked_from_unknown => Err(h),
            (Some(_), _) => Ok(self.unbox_dynamic(h, exp)),
        }
    }

    /// A typed value boxed as `exp` (`any` or `unknown`).
    fn box_dynamic(&mut self, h: hir::Expr, exp: TyId) -> Result<hir::Expr, hir::Expr> {
        let span = h.span;
        // Already dynamic (the payload of an `any | null`): the same value, never a box in a
        // box.
        if self.cx.dynamic(h.ty).is_some() {
            let mut h = h;
            if is_place(&h) && outer_mode(&h) == Some(UseMode::Borrow) {
                // A borrowed payload: a share of it.
                let ty = h.ty;
                h = self.intrinsic(Intrinsic::Share, vec![h], ty, span);
            } else {
                self.force_move(&mut h);
            }
            return Ok(if h.ty == exp {
                h
            } else {
                self.unbox_dynamic(h, exp)
            });
        }
        if let Some(v) = self.cx.lit_value(h.ty) {
            let b = self.lit_to_base(h, &v);
            return self.box_dynamic(b, exp);
        }
        if self.cx.union_def(h.ty).is_some() {
            return self.union_to_common(h, exp);
        }
        if let Some(payload) = self.cx.ty.opt_payload(h.ty) {
            if !matches!(h.kind, H::Lit(hir::Lit::Null)) {
                return self.option_to_dynamic(h, payload, exp);
            }
        }
        match self.cx.ty.kind(h.ty) {
            TyKind::Unit | TyKind::Never | TyKind::Error | TyKind::Param(_) => return Err(h),
            TyKind::Promise(..) => return Err(h),
            _ => {}
        }
        let Some(imp) = self.cx.any_impl() else {
            return Err(h);
        };
        let mut h = h;
        self.force_move(&mut h);
        Ok(self.mk(
            H::ToDyn {
                expr: Box::new(h),
                impl_index: imp,
            },
            exp,
            span,
        ))
    }

    /// `null` or `undefined` as a dynamic value of type `exp`.
    pub(crate) fn dynamic_nothing(&mut self, n: Nothing, exp: TyId, span: Span) -> hir::Expr {
        let t = self.cx.dynamic_nothing_ty(n);
        let null = self.mk(H::Lit(hir::Lit::Null), t, span);
        match self.box_dynamic(null, exp) {
            Ok(h) | Err(h) => h,
        }
    }

    /// `T | null` boxed: its payload, or the dynamic `null` / `undefined`.
    fn option_to_dynamic(
        &mut self,
        h: hir::Expr,
        payload: TyId,
        exp: TyId,
    ) -> Result<hir::Expr, hir::Expr> {
        let span = h.span;
        let nothing = self.cx.ty.nothing_of(h.ty).unwrap_or(Nothing::Null);
        let consume = !is_place(&h) || outer_mode(&h) == Some(UseMode::Move);
        let mode = if self.cx.is_copy(payload) {
            UseMode::Copy
        } else if consume {
            UseMode::Move
        } else {
            UseMode::Borrow
        };
        let b = self.new_local("<some>", payload, false, span, LocalKind::Bind);
        let value = self.mk(H::Local(b, mode), payload, span);
        let boxed = self.box_dynamic(value, exp)?;
        let none = self.dynamic_nothing(nothing, exp, span);
        let bind = hir::Pat {
            kind: P::Binding(b, mode),
            ty: payload,
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
                body: boxed,
            },
            hir::Arm {
                pat: hir::Pat {
                    kind: P::None,
                    ty: h.ty,
                    span,
                },
                guard: None,
                body: none,
            },
        ];
        let m = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        Ok(self.mk(m, exp, span))
    }

    /// A dynamic value as `exp`: checked at run time (`Intrinsic::AnyTo`).
    fn unbox_dynamic(&mut self, h: hir::Expr, exp: TyId) -> hir::Expr {
        let span = h.span;
        let members = match self.cx.union_members(exp) {
            Some(ms) => Some(ms),
            None if self.cx.lit_value(exp).is_some() => Some(vec![exp]),
            None => None,
        };
        if let Some(members) = members {
            return self.dynamic_to_members(h, exp, members);
        }
        // A variable keeps its value: the conversion takes a share of it.
        let h = if is_place(&h) {
            let ty = h.ty;
            self.intrinsic(Intrinsic::Share, vec![h], ty, span)
        } else {
            h
        };
        self.intrinsic(Intrinsic::AnyTo, vec![h], exp, span)
    }

    /// A dynamic value as the union or literal type `exp` (`members`: its members, or `exp`
    /// itself): the first member whose test the value passes (`typeof` for a primitive,
    /// `instanceof` for a class, `===` for a literal), converted; else a `TypeError`.
    fn dynamic_to_members(&mut self, h: hir::Expr, exp: TyId, members: Vec<TyId>) -> hir::Expr {
        let span = h.span;
        let dty = h.ty;
        let init = if is_place(&h) {
            self.intrinsic(Intrinsic::Share, vec![h], dty, span)
        } else {
            h
        };
        let tmp = self.new_local("<any>", dty, false, span, LocalKind::Temp);
        let name = self.cx.display(exp);
        let msg = self.str_lit(&format!("TypeError: the value is not a {name}"), span);
        let never = self.cx.ty.never;
        let mut result = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        for &m in members.iter().rev() {
            let Some((test, value)) = self.member_test(tmp, dty, m, span) else {
                let d = Diagnostic::error(
                    format!(
                        "converting an `any` to `{name}` is not supported yet (#924): no run-time test for `{}`",
                        self.cx.display(m)
                    ),
                    span,
                )
                .with_note("test the value first (`typeof`, `instanceof`) and convert it to that member");
                self.cx.error(d);
                return self.error_expr(span);
            };
            let value = self.coerce(value, exp);
            let kind = H::If {
                cond: Box::new(test),
                then: Box::new(value),
                els: Box::new(result),
            };
            result = self.mk(kind, exp, span);
        }
        let block = hir::Block {
            stmts: vec![hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(init),
                },
                span,
            }],
            value: Some(Box::new(result)),
            span,
        };
        self.mk(H::Block(block), exp, span)
    }

    /// The test that the dynamic value in `tmp` (type `dty`) holds a `m`, and the value as `m`.
    fn member_test(
        &mut self,
        tmp: LocalId,
        dty: TyId,
        m: TyId,
        span: Span,
    ) -> Option<(hir::Expr, hir::Expr)> {
        let b = self.cx.ty.bool_;
        let read = |s: &mut Self| s.mk(H::Local(tmp, UseMode::Borrow), dty, span);
        if let Some(v) = self.cx.lit_value(m) {
            let base = self.base_lit_expr(&v, span);
            let any = self.any_ty()?;
            let boxed = self.coerce(base, any);
            let x = read(self);
            let test = self.intrinsic(Intrinsic::AnyStrictEq, vec![x, boxed], b, span);
            return Some((test, self.lit_const(m, span)));
        }
        let test = match self.cx.ty.kind(m) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Str | TyKind::Bool => {
                let tag = self.cx.typeof_tag(m);
                let str_ = self.cx.ty.str_;
                let x = read(self);
                let t = self.intrinsic(Intrinsic::AnyTypeof, vec![x], str_, span);
                let want = self.str_lit(tag, span);
                let kind = H::Binary {
                    op: hir::BinOp::Eq,
                    lhs: Box::new(t),
                    rhs: Box::new(want),
                };
                self.mk(kind, b, span)
            }
            _ => {
                let (class, _) = self.cx.class_of(m)?;
                let arms = vec![
                    hir::Arm {
                        pat: self.pat(P::InstanceOf(class), dty, span),
                        guard: None,
                        body: self.mk(H::Lit(hir::Lit::Bool(true)), b, span),
                    },
                    hir::Arm {
                        pat: self.pat(P::Wildcard, dty, span),
                        guard: None,
                        body: self.mk(H::Lit(hir::Lit::Bool(false)), b, span),
                    },
                ];
                let kind = H::Match {
                    scrutinee: Box::new(read(self)),
                    arms,
                };
                self.mk(kind, b, span)
            }
        };
        let x = read(self);
        let share = self.intrinsic(Intrinsic::Share, vec![x], dty, span);
        let value = self.intrinsic(Intrinsic::AnyTo, vec![share], m, span);
        Some((test, value))
    }

    /// The error for an `unknown` used where a typed value is needed.
    pub(crate) fn unknown_use_error(&mut self, span: Span) {
        self.cx.error(
            Diagnostic::error("an `unknown` value must be narrowed before use", span)
                .with_note("test it with `typeof`, `instanceof` or `=== null` first, or convert it with `as T` (checked at run time)"),
        );
    }
}

impl FnCx<'_, '_> {
    /// The type `any` (the prelude's interface); `None` without a prelude.
    pub(crate) fn any_ty(&mut self) -> Option<TyId> {
        let defs = self.cx.any?;
        Some(self.cx.ty.intern(TyKind::Dyn(defs.any, vec![])))
    }

    /// The variable of this function `e` names, when its type is `any` or `unknown`.
    pub(crate) fn own_dynamic_local(&mut self, e: &velt_syntax::ast::Expr) -> Option<LocalId> {
        use velt_syntax::ast::ExprKind as A;
        let name = match &e.kind {
            A::Paren(x) => return self.own_dynamic_local(x),
            A::Ident(i) => i.name.as_str(),
            _ => return None,
        };
        let l = self
            .f
            .scopes
            .iter()
            .rev()
            .find_map(|s| s.names.get(name).copied())?;
        self.cx.dynamic(self.local_ty(l)).map(|_| l)
    }

    /// `s === null` / `s == undefined` (`word`) on a dynamic value `s`.
    pub(crate) fn dynamic_null_compare(
        &mut self,
        s: hir::Expr,
        word: &str,
        strict: bool,
        is_eq: bool,
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let n = if word == "undefined" {
            Nothing::Undefined
        } else {
            Nothing::Null
        };
        let l = self.dynamic_operand(s, any);
        let r = self.dynamic_nothing(n, any, span);
        let i = if strict {
            Intrinsic::AnyStrictEq
        } else {
            Intrinsic::AnyLooseEq
        };
        let b = self.cx.ty.bool_;
        let test = self.intrinsic(i, vec![l, r], b, span);
        self.negated(test, !is_eq, span)
    }

    /// An operand of a dynamic comparison: an `any` or `unknown` as it is (the comparison
    /// only reads it), anything else boxed.
    fn dynamic_operand(&mut self, h: hir::Expr, any: TyId) -> hir::Expr {
        if self.cx.dynamic(h.ty).is_some() {
            h
        } else {
            self.coerce(h, any)
        }
    }

    /// `l === r` / `l == r` (`negate`: `!==` / `!=`) where the checked type of an operand is
    /// `any` or `unknown`: both sides boxed (`Intrinsic::AnyStrictEq` / `AnyLooseEq`).
    pub(crate) fn dynamic_eq(
        &mut self,
        l: hir::Expr,
        r: hir::Expr,
        strict: bool,
        negate: bool,
        span: Span,
    ) -> hir::Expr {
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let l = self.dynamic_operand(l, any);
        let r = self.dynamic_operand(r, any);
        let i = if strict {
            Intrinsic::AnyStrictEq
        } else {
            Intrinsic::AnyLooseEq
        };
        let b = self.cx.ty.bool_;
        let test = self.intrinsic(i, vec![l, r], b, span);
        self.negated(test, negate, span)
    }
}

impl FnCx<'_, '_> {
    /// An operator other than `==` / `===` with an `any` or `unknown` operand (phase 2 of
    /// #924 for `any`; `unknown` must be narrowed first).
    pub(crate) fn dynamic_operator(
        &mut self,
        op: &str,
        l: &hir::Expr,
        r: &hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if [l.ty, r.ty]
            .iter()
            .any(|t| self.cx.dynamic(*t) == Some(Dynamic::Unknown))
        {
            self.unknown_use_error(span);
        } else {
            self.cx.error(
                Diagnostic::error(
                    format!("the operator `{op}` on an `any` value is not supported yet (#924)"),
                    span,
                )
                .with_note("convert the value to a typed one first: `(x as number) + 1`"),
            );
        }
        self.error_expr(span)
    }
}
