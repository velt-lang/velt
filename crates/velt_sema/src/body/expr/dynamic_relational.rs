//! `<`, `>`, `<=` and `>=` with an `any` operand (#924): JavaScript's relational comparison.
//! Two strings compare by their UTF-16 code units; any other pair compares as numbers, each
//! converted with `ToNumber` (`true` is 1, `null` is 0, `undefined` and a string that is not a
//! number are `NaN`, and a comparison with `NaN` is `false`). A symbol stops the program with
//! Node's `TypeError`; an object or a function (`ToPrimitive`, which calls `valueOf` /
//! `toString`) stops it with a `TypeError` saying it is not supported yet, never a wrong answer.

use velt_common::Span;

use crate::any::Dynamic;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, BinOp, ExprKind as H, Intrinsic, LocalId, TyId, UseMode};

/// One operand of the comparison, bound to a temporary.
#[derive(Clone, Copy)]
enum Side {
    /// An `any`.
    Dyn(LocalId, TyId),
    /// A number (`f64`).
    Num(LocalId),
    /// A string.
    Str(LocalId),
}

impl FnCx<'_, '_> {
    /// Neither operand is an `unknown` (which must be narrowed before use).
    pub(crate) fn no_unknown_operand(&self, l: &hir::Expr, r: &hir::Expr) -> bool {
        [l.ty, r.ty]
            .iter()
            .all(|t| self.cx.dynamic(*t) != Some(Dynamic::Unknown))
    }

    /// `l op r` (`op` one of `<`, `>`, `<=`, `>=`) where an operand is an `any`.
    pub(crate) fn dynamic_relational(
        &mut self,
        op: BinOp,
        l: hir::Expr,
        r: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let mut stmts = Vec::new();
        let (Some(a), Some(b)) = (self.rel_side(l, &mut stmts), self.rel_side(r, &mut stmts))
        else {
            return self.error_expr(span);
        };
        let result = match (a, b) {
            (Side::Num(x), Side::Num(y)) => {
                let (x, y) = (self.rel_num_read(x, span), self.rel_num_read(y, span));
                self.rel_compare(op, x, y, span)
            }
            (Side::Str(x), Side::Str(y)) => {
                let (x, y) = (self.rel_str_read(x, span), self.rel_str_read(y, span));
                self.rel_compare(op, x, y, span)
            }
            _ => {
                // Strings on both sides compare as strings, anything else as numbers.
                let both = self.rel_both_strings(a, b, span);
                let numbers = {
                    let (x, y) = (self.rel_side_number(a, span), self.rel_side_number(b, span));
                    self.rel_compare(op, x, y, span)
                };
                match both {
                    None => numbers,
                    Some(test) => {
                        let (x, y) = (self.rel_side_str(a, span), self.rel_side_str(b, span));
                        let strings = self.rel_compare(op, x, y, span);
                        let bool_ = self.cx.ty.bool_;
                        let kind = H::If {
                            cond: Box::new(test),
                            then: Box::new(strings),
                            els: Box::new(numbers),
                        };
                        self.mk(kind, bool_, span)
                    }
                }
            }
        };
        let block = hir::Block {
            stmts,
            value: Some(Box::new(result)),
            span,
        };
        let bool_ = self.cx.ty.bool_;
        self.mk(H::Block(block), bool_, span)
    }

    /// Binds the operand `h` to a temporary (operands are evaluated left to right, once).
    fn rel_side(&mut self, h: hir::Expr, stmts: &mut Vec<hir::Stmt>) -> Option<Side> {
        let span = h.span;
        if self.cx.ty.is_bottom(h.ty) {
            return None;
        }
        let (str_, f64_) = (self.cx.ty.str_, self.cx.ty.f64);
        let h = self.unbrand(h);
        let wide = self.cx.widened(h.ty);
        let (side, init) = if self.cx.dynamic(h.ty) == Some(Dynamic::Any) {
            let ty = h.ty;
            let tmp = self.new_local("<any>", ty, false, span, LocalKind::Temp);
            (Side::Dyn(tmp, ty), self.share_if_place(h))
        } else if wide == str_ {
            let h = self.coerce(h, str_);
            let tmp = self.new_local("<str>", str_, false, span, LocalKind::Temp);
            (Side::Str(tmp), self.share_if_place(h))
        } else if self.cx.ty.is_numeric(wide) {
            let h = self.coerce_number(h, f64_);
            let tmp = self.new_local("<num>", f64_, false, span, LocalKind::Temp);
            (Side::Num(tmp), h)
        } else if wide == self.cx.ty.bool_ {
            let one = self.mk(H::Lit(hir::Lit::Float(1.0)), f64_, span);
            let zero = self.mk(H::Lit(hir::Lit::Float(0.0)), f64_, span);
            let kind = H::If {
                cond: Box::new(h),
                then: Box::new(one),
                els: Box::new(zero),
            };
            let h = self.mk(kind, f64_, span);
            let tmp = self.new_local("<num>", f64_, false, span, LocalKind::Temp);
            (Side::Num(tmp), h)
        } else {
            // `null`, `undefined`, an object: as the `any` it converts to.
            let any = self.any_ty()?;
            let h = self.coerce(h, any);
            let tmp = self.new_local("<any>", any, false, span, LocalKind::Temp);
            (Side::Dyn(tmp, any), h)
        };
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local: match side {
                    Side::Dyn(t, _) | Side::Num(t) | Side::Str(t) => t,
                },
                init: Some(init),
            },
            span,
        });
        Some(side)
    }

    fn rel_compare(&mut self, op: BinOp, l: hir::Expr, r: hir::Expr, span: Span) -> hir::Expr {
        let kind = H::Binary {
            op,
            lhs: Box::new(l),
            rhs: Box::new(r),
        };
        let bool_ = self.cx.ty.bool_;
        self.mk(kind, bool_, span)
    }

    fn rel_num_read(&mut self, t: LocalId, span: Span) -> hir::Expr {
        let f64_ = self.cx.ty.f64;
        self.mk(H::Local(t, UseMode::Borrow), f64_, span)
    }

    fn rel_str_read(&mut self, t: LocalId, span: Span) -> hir::Expr {
        let str_ = self.cx.ty.str_;
        let read = self.mk(H::Local(t, UseMode::Borrow), str_, span);
        self.intrinsic(Intrinsic::Share, vec![read], str_, span)
    }

    /// `typeof x === tag` of the `any` in `t`.
    fn rel_typeof_is(&mut self, t: LocalId, dty: TyId, tag: &str, span: Span) -> hir::Expr {
        let (str_, bool_) = (self.cx.ty.str_, self.cx.ty.bool_);
        let read = self.mk(H::Local(t, UseMode::Borrow), dty, span);
        let of = self.intrinsic(Intrinsic::AnyTypeof, vec![read], str_, span);
        let want = self.str_lit(tag, span);
        let kind = H::Binary {
            op: BinOp::Eq,
            lhs: Box::new(of),
            rhs: Box::new(want),
        };
        self.mk(kind, bool_, span)
    }

    /// The value of the `any` in `t` as `ty` (its `typeof` was tested).
    fn rel_any_as(&mut self, t: LocalId, dty: TyId, ty: TyId, span: Span) -> hir::Expr {
        let read = self.mk(H::Local(t, UseMode::Borrow), dty, span);
        let share = self.intrinsic(Intrinsic::Share, vec![read], dty, span);
        self.intrinsic(Intrinsic::AnyTo, vec![share], ty, span)
    }

    /// Whether both operands are strings, or `None` when one is a number.
    fn rel_both_strings(&mut self, a: Side, b: Side, span: Span) -> Option<hir::Expr> {
        let test = |s: &mut Self, side: Side| match side {
            Side::Dyn(t, d) => Some(Some(s.rel_typeof_is(t, d, "string", span))),
            Side::Str(_) => Some(None),
            Side::Num(_) => None,
        };
        let (x, y) = (test(self, a)?, test(self, b)?);
        Some(match (x, y) {
            (Some(x), Some(y)) => {
                let bool_ = self.cx.ty.bool_;
                let f = self.mk(H::Lit(hir::Lit::Bool(false)), bool_, span);
                let kind = H::If {
                    cond: Box::new(x),
                    then: Box::new(y),
                    els: Box::new(f),
                };
                self.mk(kind, bool_, span)
            }
            (Some(x), None) | (None, Some(x)) => x,
            (None, None) => unreachable!("ICE: two string operands take the string path"),
        })
    }

    /// The operand as a string (both operands were tested to be strings).
    fn rel_side_str(&mut self, side: Side, span: Span) -> hir::Expr {
        match side {
            Side::Str(t) => self.rel_str_read(t, span),
            Side::Dyn(t, d) => {
                let str_ = self.cx.ty.str_;
                self.rel_any_as(t, d, str_, span)
            }
            Side::Num(_) => unreachable!("ICE: a number operand on the string path"),
        }
    }

    /// The operand as JavaScript's `ToNumber` converts it.
    fn rel_side_number(&mut self, side: Side, span: Span) -> hir::Expr {
        match side {
            Side::Num(t) => self.rel_num_read(t, span),
            Side::Str(t) => {
                let s = self.rel_str_read(t, span);
                self.rel_str_number(s, span)
            }
            Side::Dyn(t, d) => self.rel_any_number(t, d, span),
        }
    }

    /// `Number(s)` of a string (std's `Number`).
    fn rel_str_number(&mut self, s: hir::Expr, span: Span) -> hir::Expr {
        let f64_ = self.cx.ty.f64;
        let Some(d) = self.cx.prelude_fn("Number") else {
            return self.error_expr(span);
        };
        let kind = H::Call {
            callee: hir::Callee::Def(d, vec![]),
            args: vec![s],
        };
        self.mk(kind, f64_, span)
    }

    /// `ToNumber` of the `any` in `t`, tested by its `typeof`.
    fn rel_any_number(&mut self, t: LocalId, d: TyId, span: Span) -> hir::Expr {
        let (f64_, str_, bool_) = (self.cx.ty.f64, self.cx.ty.str_, self.cx.ty.bool_);
        let never = self.cx.ty.never;
        let msg = self.str_lit(
            "TypeError: comparing an object or a function with `<`, `>`, `<=` or `>=` through `any` is not supported yet (#924); convert it to a number or a string first",
            span,
        );
        let mut result = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        // Node's error for `Symbol() < 1`.
        let test = self.rel_typeof_is(t, d, "symbol", span);
        let msg = self.str_lit("TypeError: Cannot convert a Symbol value to a number", span);
        let stop = self.intrinsic(Intrinsic::Panic, vec![msg], never, span);
        result = self.rel_choose(test, stop, result, f64_, span);
        let read = self.mk(H::Local(t, UseMode::Borrow), d, span);
        let test = self.dynamic_null_compare(read, "null", true, true, span);
        let zero = self.mk(H::Lit(hir::Lit::Float(0.0)), f64_, span);
        result = self.rel_choose(test, zero, result, f64_, span);
        let test = self.rel_typeof_is(t, d, "undefined", span);
        let nan = self.mk(H::Lit(hir::Lit::Float(f64::NAN)), f64_, span);
        result = self.rel_choose(test, nan, result, f64_, span);
        let test = self.rel_typeof_is(t, d, "boolean", span);
        let b = self.rel_any_as(t, d, bool_, span);
        let one = self.mk(H::Lit(hir::Lit::Float(1.0)), f64_, span);
        let zero = self.mk(H::Lit(hir::Lit::Float(0.0)), f64_, span);
        let bit = self.rel_choose(b, one, zero, f64_, span);
        result = self.rel_choose(test, bit, result, f64_, span);
        let test = self.rel_typeof_is(t, d, "string", span);
        let s = self.rel_any_as(t, d, str_, span);
        let n = self.rel_str_number(s, span);
        result = self.rel_choose(test, n, result, f64_, span);
        let test = self.rel_typeof_is(t, d, "number", span);
        let n = self.rel_any_as(t, d, f64_, span);
        self.rel_choose(test, n, result, f64_, span)
    }

    fn rel_choose(
        &mut self,
        cond: hir::Expr,
        then: hir::Expr,
        els: hir::Expr,
        ty: TyId,
        span: Span,
    ) -> hir::Expr {
        let kind = H::If {
            cond: Box::new(cond),
            then: Box::new(then),
            els: Box::new(els),
        };
        self.mk(kind, ty, span)
    }

    /// `Object.is(a, b)` where an operand is an `any` (`None`: neither is): SameValue, which is
    /// `===` except that `NaN` is itself and `0` is not `-0`. Both operands are evaluated once,
    /// in order.
    pub(crate) fn object_is_dynamic(
        &mut self,
        args: &[velt_syntax::ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let [a, b] = args else {
            return None;
        };
        let dynamic = [a, b].iter().any(|e| {
            self.trial_type(e)
                .is_some_and(|t| self.cx.dynamic(t) == Some(Dynamic::Any))
        });
        if !dynamic {
            return None;
        }
        let any = self.any_ty()?;
        let mut stmts = Vec::new();
        let mut temps = Vec::new();
        for e in [a, b] {
            let h = self.expr(e, None, crate::body::Want::Borrow);
            if self.cx.ty.is_bottom(h.ty) {
                return Some(self.error_expr(span));
            }
            let h = self.unbrand(h);
            let h = if self.cx.dynamic(h.ty).is_some() {
                self.share_if_place(h)
            } else {
                self.coerce(h, any)
            };
            let ty = h.ty;
            let tmp = self.new_local("<any>", ty, false, e.span, LocalKind::Temp);
            stmts.push(hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(h),
                },
                span: e.span,
            });
            temps.push((tmp, ty));
        }
        let [(x, xt), (y, yt)] = temps[..] else {
            return None;
        };
        let (f64_, bool_) = (self.cx.ty.f64, self.cx.ty.bool_);
        let xn = self.rel_typeof_is(x, xt, "number", span);
        let yn = self.rel_typeof_is(y, yt, "number", span);
        let f = self.mk(H::Lit(hir::Lit::Bool(false)), bool_, span);
        let both = self.rel_choose(xn, yn, f, bool_, span);
        let (vx, vy) = (
            self.rel_any_as(x, xt, f64_, span),
            self.rel_any_as(y, yt, f64_, span),
        );
        let numbers = self.intrinsic(Intrinsic::SameValue, vec![vx, vy], bool_, span);
        let rx = self.mk(H::Local(x, UseMode::Borrow), xt, span);
        let ry = self.mk(H::Local(y, UseMode::Borrow), yt, span);
        let other = self.intrinsic(Intrinsic::AnyStrictEq, vec![rx, ry], bool_, span);
        let result = self.rel_choose(both, numbers, other, bool_, span);
        let block = hir::Block {
            stmts,
            value: Some(Box::new(result)),
            span,
        };
        Some(self.mk(H::Block(block), bool_, span))
    }
}
