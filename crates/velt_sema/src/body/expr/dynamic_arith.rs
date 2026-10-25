//! Arithmetic, bitwise and unary operators with an `any` operand (#924), as JavaScript runs
//! them: each operand is evaluated, then converted with `ToNumber` (`ToPrimitive` first for an
//! object). The conversion of a primitive is one call through its descriptor
//! (`Intrinsic::AnyToNumber`); an object or a function held goes to the prelude's
//! `__anyObjectNumber` (`[5]` is 5, `{}` NaN, a `Date` its time). `-`, `*`, `/`, `%`, `**` and
//! the bitwise operators give a number, as in TypeScript; `+` adds numbers when neither operand
//! holds a string, an object or a symbol (a test of the descriptors' tags), else it is the
//! prelude's `__anyAdd` (string concatenation after `ToPrimitive`), and gives an `any`.

use velt_common::Span;
use velt_syntax::ast;

use crate::any::Dynamic;
use crate::body::{FnCx, LocalKind};
use crate::hir::{self, BinOp, ExprKind as H, Intrinsic, LocalId, TyId, UseMode};

/// `Intrinsic::AnyTag` of a number; tags up to it (`undefined`, `null`, booleans, numbers) add
/// as numbers.
const TAG_NUMBER: u128 = 3;
/// `Intrinsic::AnyTag` of a function; it and objects convert with `ToPrimitive`.
pub(crate) const TAG_FUNCTION: u128 = 6;

/// One operand of a dynamic operator, bound to a temporary.
#[derive(Clone, Copy)]
pub(crate) enum Side {
    /// An `any` (an operand of another type that is no number is converted to one).
    Dyn(LocalId, TyId),
    /// A number (`f64`).
    Num(LocalId),
}

impl FnCx<'_, '_> {
    /// Binds the operand `h` to a temporary appended to `stmts` (operands are evaluated left
    /// to right, once, before either is converted).
    pub(crate) fn op_side(&mut self, h: hir::Expr, stmts: &mut Vec<hir::Stmt>) -> Option<Side> {
        let span = h.span;
        if self.cx.ty.is_bottom(h.ty) {
            return None;
        }
        let f64_ = self.cx.ty.f64;
        let h = self.unbrand(h);
        let wide = self.cx.widened(h.ty);
        let (side, init) = if self.cx.dynamic(h.ty) == Some(Dynamic::Any) {
            let ty = h.ty;
            let tmp = self.new_local("<any>", ty, false, span, LocalKind::Temp);
            (Side::Dyn(tmp, ty), self.share_if_place(h))
        } else if self.cx.ty.is_numeric(wide) {
            let h = self.coerce_number(h, f64_);
            let tmp = self.new_local("<num>", f64_, false, span, LocalKind::Temp);
            (Side::Num(tmp), h)
        } else {
            // A string, a boolean, `null`, `undefined`, an object: as the `any` it converts to.
            let any = self.any_ty()?;
            let h = self.coerce(h, any);
            let tmp = self.new_local("<any>", any, false, span, LocalKind::Temp);
            (Side::Dyn(tmp, any), h)
        };
        let local = match side {
            Side::Dyn(t, _) | Side::Num(t) => t,
        };
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Let {
                local,
                init: Some(init),
            },
            span,
        });
        Some(side)
    }

    /// A call of the prelude's operator helper `name`, which reads arrays and objects through
    /// `any` (their member slots).
    pub(crate) fn dyn_helper_call(
        &mut self,
        name: &str,
        args: Vec<hir::Expr>,
        ret: TyId,
        span: Span,
    ) -> hir::Expr {
        self.note_dyn_members();
        self.prelude_hir_call(name, args, ret, span)
    }

    /// The `any` in `t`, borrowed.
    pub(crate) fn side_any(&mut self, t: LocalId, d: TyId, span: Span) -> hir::Expr {
        self.mk(H::Local(t, UseMode::Borrow), d, span)
    }

    /// `AnyTag(t) op k` of the `any` in `t`.
    pub(crate) fn tag_test(&mut self, t: LocalId, d: TyId, op: BinOp, k: u128, span: Span) -> hir::Expr {
        let (i64_, bool_) = (self.cx.ty.i64, self.cx.ty.bool_);
        let read = self.side_any(t, d, span);
        let tag = self.intrinsic(Intrinsic::AnyTag, vec![read], i64_, span);
        let k = self.mk(H::Lit(hir::Lit::Int(k)), i64_, span);
        let kind = H::Binary {
            op,
            lhs: Box::new(tag),
            rhs: Box::new(k),
        };
        self.mk(kind, bool_, span)
    }

    /// The operand as JavaScript's `ToNumber` converts it (`ToPrimitive` first for an object).
    pub(crate) fn op_number(&mut self, s: Side, span: Span) -> hir::Expr {
        let f64_ = self.cx.ty.f64;
        let (t, d) = match s {
            Side::Num(t) => return self.mk(H::Local(t, UseMode::Copy), f64_, span),
            Side::Dyn(t, d) => (t, d),
        };
        let is_obj = self.tag_test(t, d, BinOp::GtEq, TAG_FUNCTION, span);
        let read = self.side_any(t, d, span);
        let site = self.text_site(span);
        let obj = self.dyn_helper_call("__anyObjectNumber", vec![read, site], f64_, span);
        let prim = self.op_prim_number(t, d, span);
        self.op_choose(is_obj, obj, prim, f64_, span)
    }

    /// `ToNumber` of the primitive the `any` in `t` holds: one call through its descriptor.
    pub(crate) fn op_prim_number(&mut self, t: LocalId, d: TyId, span: Span) -> hir::Expr {
        let f64_ = self.cx.ty.f64;
        let read = self.side_any(t, d, span);
        self.intrinsic(Intrinsic::AnyToNumber, vec![read], f64_, span)
    }

    pub(crate) fn op_choose(
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

    pub(crate) fn op_block(&mut self, stmts: Vec<hir::Stmt>, value: hir::Expr, span: Span) -> hir::Expr {
        let ty = value.ty;
        let block = hir::Block {
            stmts,
            value: Some(Box::new(value)),
            span,
        };
        self.mk(H::Block(block), ty, span)
    }

    /// `l op r` (`op` one of `-`, `*`, `/`, `%`, `**` and the bitwise and shift operators)
    /// where an operand is an `any`: both converted with `ToNumber`, a number.
    pub(crate) fn dynamic_arith(
        &mut self,
        op: ast::BinaryOp,
        l: hir::Expr,
        r: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if !self.no_unknown_operand(&l, &r) {
            return self.dynamic_operator(super::ops::op_str(op), &l, &r, span);
        }
        let mut stmts = Vec::new();
        let (Some(a), Some(b)) = (self.op_side(l, &mut stmts), self.op_side(r, &mut stmts)) else {
            return self.error_expr(span);
        };
        let x = self.op_number(a, span);
        let y = self.op_number(b, span);
        let v = self.binary_checked(op, x, y, span);
        self.op_block(stmts, v, span)
    }

    /// `l + r` where an operand is an `any` and neither is a string: numbers are added when
    /// both hold `undefined`, `null`, a boolean or a number, else `__anyAdd` runs JavaScript's
    /// `+` (a string concatenation when either side's `ToPrimitive` is a string). An `any`.
    pub(crate) fn dynamic_add(&mut self, l: hir::Expr, r: hir::Expr, span: Span) -> hir::Expr {
        if !self.no_unknown_operand(&l, &r) {
            return self.dynamic_operator("+", &l, &r, span);
        }
        let Some(any) = self.any_ty() else {
            return self.error_expr(span);
        };
        let mut stmts = Vec::new();
        let (Some(a), Some(b)) = (self.op_side(l, &mut stmts), self.op_side(r, &mut stmts)) else {
            return self.error_expr(span);
        };
        let bool_ = self.cx.ty.bool_;
        let mut test: Option<hir::Expr> = None;
        for s in [a, b] {
            if let Side::Dyn(t, d) = s {
                let c = self.tag_test(t, d, BinOp::LtEq, TAG_NUMBER, span);
                test = Some(match test {
                    None => c,
                    Some(p) => {
                        let f = self.mk(H::Lit(hir::Lit::Bool(false)), bool_, span);
                        self.op_choose(p, c, f, bool_, span)
                    }
                });
            }
        }
        let number = |s: &mut Self, side: Side| match side {
            Side::Num(t) => {
                let f64_ = s.cx.ty.f64;
                s.mk(H::Local(t, UseMode::Copy), f64_, span)
            }
            Side::Dyn(t, d) => s.op_prim_number(t, d, span),
        };
        let (x, y) = (number(self, a), number(self, b));
        let f64_ = self.cx.ty.f64;
        let sum = self.mk(
            H::Binary {
                op: BinOp::Add,
                lhs: Box::new(x),
                rhs: Box::new(y),
            },
            f64_,
            span,
        );
        let fast = self.coerce(sum, any);
        let as_any = |s: &mut Self, side: Side| match side {
            Side::Num(t) => {
                let n = s.mk(H::Local(t, UseMode::Copy), f64_, span);
                s.coerce(n, any)
            }
            Side::Dyn(t, d) => s.side_any(t, d, span),
        };
        let (xa, ya) = (as_any(self, a), as_any(self, b));
        let site = self.text_site(span);
        let slow = self.dyn_helper_call("__anyAdd", vec![xa, ya, site], any, span);
        let v = match test {
            Some(test) => self.op_choose(test, fast, slow, any, span),
            None => slow,
        };
        self.op_block(stmts, v, span)
    }

    /// `-x`, `+x` and `~x` of an `any` `x`: a number.
    pub(crate) fn dynamic_unary(
        &mut self,
        op: ast::UnaryOp,
        inner: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        if self.cx.dynamic(inner.ty) == Some(Dynamic::Unknown) {
            self.unknown_use_error(span);
            return self.error_expr(span);
        }
        let mut stmts = Vec::new();
        let Some(a) = self.op_side(inner, &mut stmts) else {
            return self.error_expr(span);
        };
        let x = self.op_number(a, span);
        let f64_ = self.cx.ty.f64;
        let v = match op {
            ast::UnaryOp::Neg => self.mk(
                H::Unary {
                    op: hir::UnOp::Neg,
                    expr: Box::new(x),
                },
                f64_,
                span,
            ),
            ast::UnaryOp::BitNot => self.js_bitnot(x, span),
            _ => x,
        };
        self.op_block(stmts, v, span)
    }

    /// The new value of a read-modify-write of the `any` `cur` (`x += v`, `x++`), with the
    /// expression's value: `x op= v` is `x op v` (an `any` for `+`, else a number boxed), whose
    /// value is the new one (`None`: read it back); `++x` / `x--` the number plus or minus one,
    /// whose value is the new number (prefix) or the old one converted (postfix).
    pub(crate) fn dynamic_rmw_value(
        &mut self,
        cur: hir::Expr,
        rmw: DynRmw<'_>,
        stmts: &mut Vec<hir::Stmt>,
        span: Span,
    ) -> Option<(hir::Expr, Option<hir::Expr>)> {
        let any = self.any_ty()?;
        match rmw {
            DynRmw::Assign(op, value) => {
                let v = self.expr(value, None, crate::body::Want::Borrow);
                if self.cx.ty.is_bottom(v.ty) {
                    return None;
                }
                let r = if op == ast::BinaryOp::Add {
                    self.dynamic_concat(cur, v, span)
                } else {
                    self.dynamic_arith(op, cur, v, span)
                };
                Some((self.coerce(r, any), None))
            }
            DynRmw::Update(op, prefix) => {
                let side = self.op_side(cur, stmts)?;
                let f64_ = self.cx.ty.f64;
                let n = self.op_number(side, span);
                let old = self.new_local("<old>", f64_, false, span, LocalKind::Temp);
                stmts.push(hir::Stmt {
                    kind: hir::StmtKind::Let {
                        local: old,
                        init: Some(n),
                    },
                    span,
                });
                let step = |s: &mut Self| {
                    let read = s.mk(H::Local(old, UseMode::Copy), f64_, span);
                    let one = s.mk(H::Lit(hir::Lit::Float(1.0)), f64_, span);
                    let op = match op {
                        ast::UpdateOp::Inc => BinOp::Add,
                        ast::UpdateOp::Dec => BinOp::Sub,
                    };
                    s.mk(
                        H::Binary {
                            op,
                            lhs: Box::new(read),
                            rhs: Box::new(one),
                        },
                        f64_,
                        span,
                    )
                };
                let new = step(self);
                let new = self.coerce(new, any);
                let value = if prefix {
                    step(self)
                } else {
                    self.mk(H::Local(old, UseMode::Copy), f64_, span)
                };
                Some((new, Some(value)))
            }
        }
    }
}

/// A read-modify-write of an `any` place or member.
#[derive(Clone, Copy)]
pub(crate) enum DynRmw<'a> {
    /// `x op= v` (`op` not logical).
    Assign(ast::BinaryOp, &'a ast::Expr),
    /// `++x` / `x++` (prefix?) and `--`.
    Update(ast::UpdateOp, bool),
}
