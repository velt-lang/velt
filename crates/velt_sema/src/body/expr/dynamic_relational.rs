//! `<`, `>`, `<=` and `>=` with an `any` operand (#924): JavaScript's relational comparison.
//! Two strings compare by their UTF-16 code units; any other pair compares as numbers, each
//! converted with `ToNumber` (`true` is 1, `null` is 0, `undefined` and a string that is not a
//! number are `NaN`, and a comparison with `NaN` is `false`). A symbol stops the program with
//! Node's `TypeError`; an object or a function is compared after `ToPrimitive` (which calls
//! `valueOf` / `toString`) by the prelude's `__anyRelational`.

use velt_common::Span;

use super::dynamic_arith::TAG_FUNCTION;
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
        let result = self.rel_objects(op, a, b, result, span);
        let block = hir::Block {
            stmts,
            value: Some(Box::new(result)),
            span,
        };
        let bool_ = self.cx.ty.bool_;
        self.mk(H::Block(block), bool_, span)
    }

    /// `result` unless an `any` operand holds an object or a function, which the prelude's
    /// `__anyRelational` compares after `ToPrimitive` (hint `"number"`: `[2] < 10`, a `Date`
    /// by its time).
    fn rel_objects(
        &mut self,
        op: BinOp,
        a: Side,
        b: Side,
        result: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let bool_ = self.cx.ty.bool_;
        let mut test: Option<hir::Expr> = None;
        for s in [a, b] {
            if let Side::Dyn(t, d) = s {
                let c = self.tag_test(t, d, BinOp::GtEq, TAG_FUNCTION, span);
                test = Some(match test {
                    None => c,
                    Some(p) => {
                        let tr = self.mk(H::Lit(hir::Lit::Bool(true)), bool_, span);
                        self.rel_choose(p, tr, c, bool_, span)
                    }
                });
            }
        }
        let (Some(test), Some(any)) = (test, self.any_ty()) else {
            return result;
        };
        let as_any = |s: &mut Self, side: Side| match side {
            Side::Dyn(t, d) => s.mk(H::Local(t, UseMode::Borrow), d, span),
            Side::Num(t) => {
                let n = s.rel_num_read(t, span);
                s.coerce(n, any)
            }
            Side::Str(t) => {
                let v = s.rel_str_read(t, span);
                s.coerce(v, any)
            }
        };
        let (x, y) = (as_any(self, a), as_any(self, b));
        let code = match op {
            BinOp::Lt => 0,
            BinOp::Gt => 1,
            BinOp::LtEq => 2,
            _ => 3,
        };
        let i64_ = self.cx.ty.i64;
        let code = self.mk(H::Lit(hir::Lit::Int(code)), i64_, span);
        let site = self.text_site(span);
        let args = vec![x, y, code, site];
        let slow = self.dyn_helper_call("__anyRelational", args, bool_, span);
        self.rel_choose(test, slow, result, bool_, span)
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

    /// `typeof x === tag` of the `any` in `t`: a test of its descriptor's tag.
    fn rel_typeof_is(&mut self, t: LocalId, dty: TyId, tag: &str, span: Span) -> hir::Expr {
        let k = match tag {
            "undefined" => 0,
            "boolean" => 2,
            "number" => 3,
            "string" => 4,
            "symbol" => 5,
            _ => unreachable!("ICE: no single tag for `typeof` {tag}"),
        };
        self.tag_test(t, dty, BinOp::Eq, k, span)
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

    /// `ToNumber` of the primitive the `any` in `t` holds (an object was compared by
    /// `rel_objects`).
    fn rel_any_number(&mut self, t: LocalId, d: TyId, span: Span) -> hir::Expr {
        self.op_prim_number(t, d, span)
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

    /// `Object.is(a, b)` where an operand is an `any` or an `unknown` (`None`: neither is): SameValue, which is
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
            self.trial_type(e).is_some_and(|t| {
                matches!(self.cx.dynamic(t), Some(Dynamic::Any | Dynamic::Unknown))
            })
        });
        if !dynamic {
            return None;
        }
        let any = self.any_ty()?;
        let mut stmts = Vec::new();
        let mut temps = Vec::new();
        for e in [a, b] {
            // `null` / `undefined` written as such: the dynamic nothing itself (a bare
            // `undefined` checks as `void`, which would box as another value).
            let nothing = match &super::object_builtins::strip_parens(e).kind {
                velt_syntax::ast::ExprKind::Lit(velt_syntax::ast::Lit::Undefined) => {
                    Some(hir::Nothing::Undefined)
                }
                velt_syntax::ast::ExprKind::Lit(velt_syntax::ast::Lit::Null) => {
                    Some(hir::Nothing::Null)
                }
                _ => None,
            };
            let h = match nothing {
                Some(n) => self.dynamic_nothing(n, any, e.span),
                None => self.expr(e, None, crate::body::Want::Borrow),
            };
            if self.cx.ty.is_bottom(h.ty) {
                return Some(self.error_expr(span));
            }
            let h = self.unbrand(h);
            let h = if matches!(self.cx.dynamic(h.ty), Some(Dynamic::Any | Dynamic::Unknown)) {
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

impl FnCx<'_, '_> {
    /// `l == r` of two dynamic values: `Intrinsic::AnyLooseEq`, unless one holds an object or
    /// a function, which the prelude's `__anyLooseEq` compares as JavaScript does (`[] == 0`,
    /// `[1] == "1"`: the object's `ToPrimitive` against a primitive).
    pub(crate) fn dynamic_loose_eq(&mut self, l: hir::Expr, r: hir::Expr, span: Span) -> hir::Expr {
        let bool_ = self.cx.ty.bool_;
        let mut stmts = Vec::new();
        let mut temps = Vec::new();
        for h in [l, r] {
            let (ty, hs) = (h.ty, h.span);
            let tmp = self.new_local("<any>", ty, false, hs, LocalKind::Temp);
            let init = self.share_if_place(h);
            stmts.push(hir::Stmt {
                kind: hir::StmtKind::Let {
                    local: tmp,
                    init: Some(init),
                },
                span: hs,
            });
            temps.push((tmp, ty));
        }
        let [(a, at), (b, bt)] = temps[..] else {
            return self.error_expr(span);
        };
        let oa = self.tag_test(a, at, BinOp::GtEq, TAG_FUNCTION, span);
        let ob = self.tag_test(b, bt, BinOp::GtEq, TAG_FUNCTION, span);
        let tr = self.mk(H::Lit(hir::Lit::Bool(true)), bool_, span);
        let test = self.op_choose(oa, tr, ob, bool_, span);
        let (x, y) = (self.side_any(a, at, span), self.side_any(b, bt, span));
        let site = self.text_site(span);
        let slow = self.dyn_helper_call("__anyLooseEq", vec![x, y, site], bool_, span);
        let (x, y) = (self.side_any(a, at, span), self.side_any(b, bt, span));
        let fast = self.intrinsic(Intrinsic::AnyLooseEq, vec![x, y], bool_, span);
        let v = self.op_choose(test, slow, fast, bool_, span);
        self.op_block(stmts, v, span)
    }
}
