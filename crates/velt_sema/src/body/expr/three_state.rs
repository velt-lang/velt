//! `T | null | undefined` (issue #930): the value is an option of a `T | null`, whose own
//! nothing is `undefined` (hir_encodings.md "Undefined"). A present value is
//! `Some(Some(v))`, `null` is `Some(None)` and `undefined` is `None`. These helpers build the
//! matches and values the null-handling operators need on such a type, and give the plain
//! two-level shapes for the other options.

use velt_common::Span;

use crate::any::Dynamic;
use crate::body::FnCx;
use crate::hir::{self, ExprKind as H, Nothing, PatKind as P, TyId};

impl FnCx<'_, '_> {
    /// Does `t` allow both `null` and `undefined`?
    pub(crate) fn is_three_state(&self, t: TyId) -> bool {
        self.cx.ty.nothing_of(t) == Some(Nothing::Both)
    }

    /// The value type of option `t` once both nothings are ruled out: `T` of `T | null`,
    /// `T | undefined` and `T | null | undefined`.
    pub(crate) fn present_payload(&self, t: TyId) -> Option<TyId> {
        self.cx.ty.core(t)
    }

    /// The pattern "present, with `inner` on the value" on a value of option type `sty`:
    /// `Some(inner)`, or `Some(Some(inner))` on a `T | null | undefined`.
    pub(crate) fn present_pat(&self, inner: hir::Pat, sty: TyId, span: Span) -> hir::Pat {
        if self.is_three_state(sty) {
            let mid = self
                .cx
                .ty
                .opt_payload(sty)
                .expect("ICE: three-state option without a payload");
            let mid_pat = self.pat(P::Some(Box::new(inner)), mid, span);
            return self.pat(P::Some(Box::new(mid_pat)), sty, span);
        }
        self.pat(P::Some(Box::new(inner)), sty, span)
    }

    /// The pattern for "either nothing" when it follows a `present_pat` arm: `None`, or a
    /// wildcard on a `T | null | undefined` (the arm before took every present value).
    pub(crate) fn nothing_pat(&self, sty: TyId, span: Span) -> hir::Pat {
        if self.is_three_state(sty) {
            return self.pat(P::Wildcard, sty, span);
        }
        self.pat(P::None, sty, span)
    }

    /// The pattern for exactly one nothing (`which`) of option type `sty`: `None` for its own
    /// nothing, `Some(None)` for the `null` of a `T | null | undefined`.
    pub(crate) fn one_nothing_pat(&self, which: Nothing, sty: TyId, span: Span) -> hir::Pat {
        if self.is_three_state(sty) && which == Nothing::Null {
            let mid = self
                .cx
                .ty
                .opt_payload(sty)
                .expect("ICE: three-state option without a payload");
            let none = self.pat(P::None, mid, span);
            return self.pat(P::Some(Box::new(none)), sty, span);
        }
        self.pat(P::None, sty, span)
    }

    /// `Lit::Null` typed `t`, for the nothing `which` of option type `t`: on a
    /// `T | null | undefined`, `null` is `WrapSome` of the `T | null`'s nothing.
    pub(crate) fn nothing_value(&mut self, which: Nothing, t: TyId, span: Span) -> hir::Expr {
        if self.is_three_state(t) && which == Nothing::Null {
            let mid = self
                .cx
                .ty
                .opt_payload(t)
                .expect("ICE: three-state option without a payload");
            let null = self.mk(H::Lit(hir::Lit::Null), mid, span);
            return self.mk(H::WrapSome(Box::new(null)), t, span);
        }
        self.mk(H::Lit(hir::Lit::Null), t, span)
    }

    /// A present value `v` (of the option's value type `T`) as option type `t`: `WrapSome`, or
    /// `WrapSome(WrapSome(v))` on a `T | null | undefined`.
    pub(crate) fn present_value(&mut self, v: hir::Expr, t: TyId, span: Span) -> hir::Expr {
        if self.is_three_state(t) {
            let mid = self
                .cx
                .ty
                .opt_payload(t)
                .expect("ICE: three-state option without a payload");
            let inner = self.mk(H::WrapSome(Box::new(v)), mid, span);
            return self.mk(H::WrapSome(Box::new(inner)), t, span);
        }
        self.mk(H::WrapSome(Box::new(v)), t, span)
    }

    /// The value of a present option place `base` (flow-narrowed): `UnwrapSome`, or two of them
    /// on a `T | null | undefined`; `mode` is the outer read's.
    pub(crate) fn unwrap_present(
        &mut self,
        base: hir::Expr,
        mode: hir::UseMode,
        span: Span,
    ) -> hir::Expr {
        let core = self
            .cx
            .ty
            .core(base.ty)
            .expect("ICE: unwrapping a non-option");
        if self.is_three_state(base.ty) {
            let mid = self
                .cx
                .ty
                .opt_payload(base.ty)
                .expect("ICE: three-state option without a payload");
            let inner = self.mk(
                H::UnwrapSome(Box::new(base), hir::UseMode::Borrow),
                mid,
                span,
            );
            return self.mk(H::UnwrapSome(Box::new(inner), mode), core, span);
        }
        self.mk(H::UnwrapSome(Box::new(base), mode), core, span)
    }

    /// The type of `c ? then : els` with no type from context when both arms are `null` or
    /// `undefined`: `null | undefined` (an option of `never` with both nothings) for one of
    /// each, `null` for two `null`s, as in TypeScript.
    pub(crate) fn nothings_conditional_ty(
        &mut self,
        then: &velt_syntax::ast::Expr,
        els: &velt_syntax::ast::Expr,
    ) -> Option<TyId> {
        use crate::body::narrow::nothing_word;
        match (nothing_word(then)?, nothing_word(els)?) {
            (a, b) if a != b => {
                let never = self.cx.ty.never;
                Some(self.cx.ty.option_of(never, Nothing::Both))
            }
            ("null", _) => Some(self.cx.ty.null_alone()),
            _ => None,
        }
    }

    /// `l == r` (`negate`: `!=`) where either side is a `T | null | undefined`: the other side
    /// converts to that type, then both nothings are equal (`null == undefined`), and present
    /// values compare as `===` does. Two values of one type with a type parameter in it
    /// (`T | string`) compare as `Intrinsic::LooseEq`, which lowering makes `===` where the
    /// instantiated type holds no nothing. `Err` gives the operands back otherwise.
    pub(crate) fn loose_three_state_eq(
        &mut self,
        l: hir::Expr,
        r: hir::Expr,
        negate: bool,
        span: Span,
    ) -> Result<hir::Expr, Box<(hir::Expr, hir::Expr)>> {
        // Two values of one type with a type parameter in it (`T | string`): which nothings
        // they hold depends on the instantiation (`T = unknown` holds both), so the test is
        // made there.
        let opt_param =
            l.ty == r.ty && (self.cx.ty.core(l.ty)).is_some_and(|p| self.param_shaped(p));
        if opt_param {
            // `T | undefined` (`T = null` holds both nothings) and `T | null | undefined`
            // (`T = unknown` compares the values held loosely): tested where instantiated.
            let b = self.cx.ty.bool_;
            let e = self.intrinsic(hir::Intrinsic::LooseEq, vec![l, r], b, span);
            return Ok(self.negated(e, negate, span));
        }
        if l.ty == r.ty && self.param_shaped(l.ty) {
            // `binary_checked` gives `Intrinsic::Same` (or a `Binary`) for these operands.
            let e = self.binary_checked(velt_syntax::ast::BinaryOp::Eq, l, r, span);
            let e = match e.kind {
                H::Binary {
                    op: hir::BinOp::Eq,
                    lhs,
                    rhs,
                } => {
                    let b = self.cx.ty.bool_;
                    self.intrinsic(hir::Intrinsic::LooseEq, vec![*lhs, *rhs], b, span)
                }
                H::Call {
                    callee: hir::Callee::Intrinsic(hir::Intrinsic::Same),
                    args,
                } => {
                    let b = self.cx.ty.bool_;
                    self.intrinsic(hir::Intrinsic::LooseEq, args, b, span)
                }
                kind => hir::Expr { kind, ..e },
            };
            return Ok(self.negated(e, negate, span));
        }
        let (l, r) = match (self.is_three_state(l.ty), self.is_three_state(r.ty)) {
            (false, false) => return Err(Box::new((l, r))),
            (true, false) if self.cx.ty.nothing_of(r.ty).is_some() => {
                let to = l.ty;
                match self.try_coerce(r, to) {
                    Ok(r) => (l, r),
                    Err(r) => return Err(Box::new((l, r))),
                }
            }
            (false, true) if self.cx.ty.nothing_of(l.ty).is_some() => {
                let to = r.ty;
                match self.try_coerce(l, to) {
                    Ok(l) => (l, r),
                    Err(l) => return Err(Box::new((l, r))),
                }
            }
            _ => (l, r),
        };
        if l.ty != r.ty {
            return Err(Box::new((l, r)));
        }
        let ty = l.ty;
        let mut stmts = vec![];
        // A variable or field operand keeps its value (it may be used again, also in the same
        // call's arguments): the temporary holds a share of it.
        let mut read = |s: &mut Self, h: hir::Expr, name: &str| {
            let mut h = if s.cx.is_copy(ty) {
                h
            } else {
                s.share_if_place(h)
            };
            s.force_move(&mut h);
            let t = s.new_local(name, ty, false, h.span, crate::body::LocalKind::Temp);
            stmts.push(hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: t,
                    init: Some(h),
                },
                span,
            });
            t
        };
        let a = read(self, l, "<loose_l>");
        let b = read(self, r, "<loose_r>");
        let mode = if self.cx.is_copy(ty) {
            hir::UseMode::Copy
        } else {
            hir::UseMode::Borrow
        };
        let get = |s: &Self, t| s.mk(H::Local(t, mode), ty, span);
        let a_nothing = self.null_test(get(self, a), true, span);
        let b_nothing = self.null_test(get(self, b), true, span);
        let same = self.binary_checked(
            velt_syntax::ast::BinaryOp::Eq,
            get(self, a),
            get(self, b),
            span,
        );
        let bool_ = self.cx.ty.bool_;
        let kind = H::If {
            cond: Box::new(a_nothing),
            then: Box::new(b_nothing),
            els: Box::new(same),
        };
        let e = self.mk(kind, bool_, span);
        let e = self.negated(e, negate, span);
        Ok(self.with_temps_value(stmts, e))
    }

    /// A destructuring default, `value` or `default` where `value` is `undefined`
    /// (`pattern_defaults::UNDEFINED_OR`): `value ?? default`, except that on a
    /// `T | null | undefined` a `null` stays (the default replaces only `undefined`).
    pub(crate) fn undefined_or(
        &mut self,
        value: &velt_syntax::ast::Expr,
        default: &velt_syntax::ast::Expr,
        exp: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let mut s = self.expr(value, None, crate::body::Want::Borrow);
        // On an `any` (a variable, read again): `value === undefined ? default : value`.
        if self.cx.dynamic(s.ty).is_some()
            && matches!(value.kind, velt_syntax::ast::ExprKind::Ident(_))
        {
            use velt_syntax::ast::{self, ExprKind as A};
            let mk = |kind| ast::Expr {
                id: ast::NodeId(u32::MAX),
                kind,
                span,
            };
            let test = mk(A::Binary {
                op: ast::BinaryOp::StrictEq,
                lhs: Box::new(value.clone()),
                rhs: Box::new(mk(A::Lit(ast::Lit::Undefined))),
            });
            let cond = mk(A::Cond {
                cond: Box::new(test),
                then: Box::new(mk(A::Cast {
                    expr: Box::new(default.clone()),
                    ty: ast::TypeExpr {
                        kind: ast::TypeExprKind::Named {
                            path: vec![ast::Ident {
                                name: "any".into(),
                                span,
                            }],
                            args: vec![],
                        },
                        span,
                    },
                })),
                els: Box::new(value.clone()),
            });
            return self.expr(&cond, None, crate::body::Want::Move);
        }
        if !self.is_three_state(s.ty) {
            return self.nullish_checked(s, default, exp, span);
        }
        let mid = self
            .cx
            .ty
            .opt_payload(s.ty)
            .expect("ICE: three-state option without a payload");
        let (l, mode) = self.option_binding(&s, mid, "<default>", true);
        if mode == hir::UseMode::Move {
            self.force_move(&mut s);
        }
        let hint = exp.unwrap_or(mid);
        let d = self.expr(default, Some(hint), crate::body::Want::Move);
        let v = self.mk(H::Local(l, mode), mid, span);
        let (v, d, ty) = self.nullish_join(v, d, mid);
        let sty = s.ty;
        let bind = self.pat(P::Binding(l, mode), mid, span);
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Some(Box::new(bind)), sty, span),
                guard: None,
                body: v,
            },
            hir::Arm {
                pat: self.pat(P::None, sty, span),
                guard: None,
                body: d,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(s),
            arms,
        };
        self.mk(kind, ty, span)
    }

    /// A `T | undefined` value `h` as the `T | null | undefined` `exp` (same `T`): a match,
    /// since the two keep `undefined` in different places.
    pub(crate) fn undefined_to_three_state(&mut self, mut h: hir::Expr, exp: TyId) -> hir::Expr {
        let span = h.span;
        let core = self
            .cx
            .ty
            .core(h.ty)
            .expect("ICE: `T | undefined` without a payload");
        let (l, mode) = self.option_binding(&h, core, "<undefined>", true);
        if mode == hir::UseMode::Move {
            self.force_move(&mut h);
        }
        let v = self.mk(H::Local(l, mode), core, span);
        let present = self.present_value(v, exp, span);
        let none = self.nothing_value(Nothing::Undefined, exp, span);
        let bind = self.pat(P::Binding(l, mode), core, span);
        let hty = h.ty;
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Some(Box::new(bind)), hty, span),
                guard: None,
                body: present,
            },
            hir::Arm {
                pat: self.pat(P::None, hty, span),
                guard: None,
                body: none,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, exp, span)
    }

    /// A two-state option value `h` (`T | undefined` or `T | null`) as the option of the same
    /// `T` with nothing `to`: its nothing becomes the other one. For places where both mean "no
    /// value" and the target keeps only one, such as a JSX attribute (`title={props.title}`
    /// with `title?: string` leaves the attribute out either way).
    pub(crate) fn with_nothing(&mut self, mut h: hir::Expr, to: Nothing) -> hir::Expr {
        let span = h.span;
        let core = self
            .cx
            .ty
            .core(h.ty)
            .expect("ICE: an option without a payload");
        let exp = self.cx.ty.option_of(core, to);
        let (l, mode) = self.option_binding(&h, core, "<nothing>", true);
        if mode == hir::UseMode::Move {
            self.force_move(&mut h);
        }
        let v = self.mk(H::Local(l, mode), core, span);
        let present = self.mk(H::WrapSome(Box::new(v)), exp, span);
        let none = self.mk(H::Lit(hir::Lit::Null), exp, span);
        let bind = self.pat(P::Binding(l, mode), core, span);
        let hty = h.ty;
        let arms = vec![
            hir::Arm {
                pat: self.pat(P::Some(Box::new(bind)), hty, span),
                guard: None,
                body: present,
            },
            hir::Arm {
                pat: self.pat(P::None, hty, span),
                guard: None,
                body: none,
            },
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, exp, span)
    }

    /// `null` / `undefined` (`which`) as a value of type `t` that allows it only through a
    /// member of its union: `T | string | undefined` at `T = number | null` holds `null` as its
    /// `T` member (#1268). The instance keeps the generic union's layout (velt_vir
    /// `canon_union`), so the value is that member's variant holding its nothing.
    pub(crate) fn nothing_in_member(
        &mut self,
        which: Nothing,
        t: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        let allows = |n: Nothing| match which {
            Nothing::Undefined => n.has_undefined(),
            _ => n.has_null(),
        };
        let (core, outer) = match self.cx.ty.nothing_of(t) {
            // The option's own nothing holds it (or the payload is a `T | null`).
            Some(n) if allows(n) || n == Nothing::Both => return None,
            Some(_) => (self.cx.ty.opt_payload(t)?, true),
            None => (t, false),
        };
        let (def, type_args) = self.cx.union_def(core)?;
        let members = self.cx.union_members(core)?;
        let held = members
            .iter()
            .position(|m| self.cx.ty.nothing_of(*m).is_some_and(allows));
        // Else an `any` / `unknown` member holds it (`T | Mod<T>` at `T = any`).
        let dynamic = || {
            members
                .iter()
                .position(|m| matches!(self.cx.dynamic(*m), Some(Dynamic::Any | Dynamic::Unknown)))
        };
        let (v, inner) = match held {
            Some(v) => (v, self.nothing_value(which, members[v], span)),
            None => {
                let v = dynamic()?;
                (v, self.dynamic_nothing(which, members[v], span))
            }
        };
        let kind = H::Variant {
            def,
            type_args,
            variant: v as u32,
            args: vec![inner],
        };
        let value = self.mk(kind, core, span);
        Some(match outer {
            true => self.mk(H::WrapSome(Box::new(value)), t, span),
            false => value,
        })
    }

    /// A `T | null | undefined` value `h` known not to be `null` (`x === null` ruled out by a
    /// test, #1218) as the `T | undefined` it reads as: a match whose `null` arm is unreachable.
    /// `consume`: the value moves out of a place.
    pub(crate) fn three_state_without_null(
        &mut self,
        mut h: hir::Expr,
        consume: bool,
    ) -> hir::Expr {
        let span = h.span;
        let hty = h.ty;
        let core = self
            .cx
            .ty
            .core(hty)
            .expect("ICE: `T | null | undefined` without a payload");
        let exp = self.cx.ty.option_of(core, Nothing::Undefined);
        let (l, mode) = self.option_binding(&h, core, "<defined>", consume);
        if mode == hir::UseMode::Move {
            self.force_move(&mut h);
        }
        let v = self.mk(H::Local(l, mode), core, span);
        let present = self.mk(H::WrapSome(Box::new(v)), exp, span);
        let bind = self.pat(P::Binding(l, mode), core, span);
        let arms = vec![
            hir::Arm {
                pat: self.present_pat(bind, hty, span),
                guard: None,
                body: present,
            },
            hir::Arm {
                pat: self.one_nothing_pat(Nothing::Undefined, hty, span),
                guard: None,
                body: self.mk(H::Lit(hir::Lit::Null), exp, span),
            },
            self.unreachable_arm(hty, span),
        ];
        let kind = H::Match {
            scrutinee: Box::new(h),
            arms,
        };
        self.mk(kind, exp, span)
    }
}
