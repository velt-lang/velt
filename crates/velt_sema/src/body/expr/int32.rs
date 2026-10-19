//! JavaScript's 32-bit integer operators on numbers (issue #521; design #525).
//!
//! When their operands are numbers (`f64`, or a type that converts to one exactly next to a
//! number), `| & ^ << >> ~` apply ToInt32 to the operands and `>>>` applies ToUint32. They
//! compile to 32-bit integer operations:
//! - a number goes through the runtime's ToInt32 (`velt_rt_math_to_int32`), which the backends
//!   emit inline (one conversion on the common path), and which `numrep` drops where it proves
//!   the value an int32 already;
//! - a product inside an operand is rounded the way JS's double multiply rounds it past 2^53:
//!   `(y * k) | 0` goes through the prelude's `__mulToInt32`, which is one 32-bit multiply for
//!   int32 operands;
//! - an integer operand of 32 bits or fewer contributes its value (`as i32`);
//! - shift counts are taken modulo 32, and the result is a number: the `i32` (or the `u32` of
//!   `>>>`) converted to `f64`, which `numrep` keeps as an integer.
//!
//! `Math.imul` and `Math.clz32` compile the same way, to one multiply or count. Operands of a
//! declared integer type keep their own (Rust) semantics: `n >>> 3` with `n: i64` is a 64-bit
//! shift.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, BinOp, ExprKind as H, IntTy, TyId, TyKind, UnOp};

/// The value of a float literal, possibly negated.
fn literal_value(h: &hir::Expr) -> Option<f64> {
    match &h.kind {
        H::Lit(hir::Lit::Float(v)) => Some(*v),
        H::Unary {
            op: UnOp::Neg,
            expr,
        } => literal_value(expr).map(|v| -v),
        _ => None,
    }
}

/// JS's ToInt32 of `x`: truncated, modulo 2^32, in the signed 32-bit range (NaN and ±∞ are 0).
fn js_to_int32(x: f64) -> i32 {
    if !x.is_finite() {
        return 0;
    }
    x.trunc().rem_euclid(4_294_967_296.0) as u32 as i32
}

fn bitwise_op(op: ast::BinaryOp) -> Option<BinOp> {
    use ast::BinaryOp as B;
    Some(match op {
        B::BitAnd => BinOp::BitAnd,
        B::BitOr => BinOp::BitOr,
        B::BitXor => BinOp::BitXor,
        B::Shl => BinOp::Shl,
        B::Shr => BinOp::Shr,
        B::UShr => BinOp::UShr,
        _ => return None,
    })
}

impl FnCx<'_, '_> {
    /// Are `l` and `r` numbers for the int32 operators: at least one is a `number`, and the
    /// other one is too or converts to one exactly (`x | k` with `k: i32`)?
    fn js_numbers(&self, l: &hir::Expr, r: &hir::Expr) -> bool {
        let f64_ = self.cx.ty.f64;
        let number = |h: &hir::Expr| h.ty == f64_ || self.exact_in_number(h.ty);
        (l.ty == f64_ || r.ty == f64_) && number(l) && number(r)
    }

    /// `l op r` with JS semantics when `op` is a bitwise operator and both operands are numbers
    /// (`js_bitwise_applies`).
    pub(super) fn js_bitwise(
        &mut self,
        op: BinOp,
        l: hir::Expr,
        r: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        self.int32_binary(op, l, r, span)
    }

    /// The operator `l op r` takes JS's 32-bit semantics: `op` is bitwise and both operands are
    /// numbers.
    pub(super) fn js_bitwise_applies(
        &self,
        op: ast::BinaryOp,
        l: &hir::Expr,
        r: &hir::Expr,
    ) -> Option<BinOp> {
        let bop = bitwise_op(op)?;
        self.js_numbers(l, r).then_some(bop)
    }

    /// `~x` takes JS's 32-bit semantics: `x` is a number.
    pub(super) fn js_bitnot_applies(&self, x: &hir::Expr) -> bool {
        x.ty == self.cx.ty.f64
    }

    /// `~x` with JS semantics (`js_bitnot_applies` holds).
    pub(super) fn js_bitnot(&mut self, x: hir::Expr, span: Span) -> hir::Expr {
        let i32_ = self.cx.ty.i32;
        let v = self.int32_of(x);
        let not = self.mk(
            H::Unary {
                op: UnOp::BitNot,
                expr: Box::new(v),
            },
            i32_,
            span,
        );
        self.widen32(not)
    }

    /// `place op= value` takes JS semantics (`js_bitwise_assign`): `op` is bitwise and both
    /// the place and the value are numbers. The value back either way.
    pub(super) fn js_bitwise_operands(
        &self,
        op: ast::BinaryOp,
        place: &hir::Expr,
        value: hir::Expr,
    ) -> Result<hir::Expr, hir::Expr> {
        let number_place = place.ty == self.cx.ty.f64;
        if bitwise_op(op).is_some() && number_place && self.js_numbers(place, &value) {
            Ok(value)
        } else {
            Err(value)
        }
    }

    /// The value of `place op= value` (`js_bitwise_operands` holds; `cur` reads the place):
    /// `place op value` with JS semantics (`x |= 0` stores ToInt32 of `x`).
    pub(super) fn js_bitwise_assign(
        &mut self,
        op: ast::BinaryOp,
        cur: hir::Expr,
        value: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let bop = bitwise_op(op).expect("ICE: js_bitwise_assign on a non-bitwise operator");
        self.int32_binary(bop, cur, value, span)
    }

    /// `Math.imul(a, b)` and `Math.clz32(x)` on the prelude's `Math`: one 32-bit multiply or
    /// leading-zero count. `None` if the call is anything else.
    pub(crate) fn math_int32_call(
        &mut self,
        callee: &ast::Expr,
        args: &[ast::Expr],
        span: Span,
    ) -> Option<hir::Expr> {
        let ast::ExprKind::Member {
            object,
            prop,
            optional: false,
        } = &callee.kind
        else {
            return None;
        };
        let ast::ExprKind::Ident(m) = &object.kind else {
            return None;
        };
        let arity = match prop.name.as_str() {
            "imul" => 2,
            "clz32" => 1,
            _ => return None,
        };
        if m.name != "Math" || args.len() != arity || self.is_local_name("Math") {
            return None;
        }
        let math = self.cx.prelude_adt("Math")?;
        match self.cx.lookup_item_at(self.module, "Math", m.span) {
            Some(crate::ctx::Item::Def(d)) if d == math => {}
            _ => return None,
        }
        let mut vals = Vec::with_capacity(arity);
        for a in args {
            let v = self.expr(a, None, Want::Borrow);
            if v.ty != self.cx.ty.f64 && !self.exact_in_number(v.ty) {
                if !self.cx.ty.is_bottom(v.ty) {
                    let found = self.cx.display(v.ty);
                    self.cx.error(
                        Diagnostic::error("mismatched types", a.span)
                            .with_note(format!("expected number, found {found}")),
                    );
                }
                return Some(self.error_expr(span));
            }
            self.literal_use_number(&v);
            vals.push(self.int32_of(v));
        }
        let i32_ = self.cx.ty.i32;
        let r = if arity == 2 {
            let b = vals.pop()?;
            let a = vals.pop()?;
            self.mk(
                H::Binary {
                    op: BinOp::Mul,
                    lhs: Box::new(a),
                    rhs: Box::new(b),
                },
                i32_,
                span,
            )
        } else {
            self.helper_call("velt_rt_math_clz32", vals, i32_, span)?
        };
        Some(self.widen32(r))
    }

    /// `l op r` on the 32-bit values of two numbers.
    fn int32_binary(&mut self, op: BinOp, l: hir::Expr, r: hir::Expr, span: Span) -> hir::Expr {
        let t = if op == BinOp::UShr {
            self.cx.ty.intern(TyKind::Int(IntTy::U32))
        } else {
            self.cx.ty.i32
        };
        let a = self.int32_of(l);
        let a = self.int_as(a, t);
        let b = self.int32_of(r);
        let b = self.int_as(b, t);
        let v = self.mk(
            H::Binary {
                op,
                lhs: Box::new(a),
                rhs: Box::new(b),
            },
            t,
            span,
        );
        self.widen32(v)
    }

    /// A 32-bit result as the number it is in JS.
    pub(super) fn widen32(&mut self, v: hir::Expr) -> hir::Expr {
        let (f64_, span) = (self.cx.ty.f64, v.span);
        self.mk(H::Cast(Box::new(v)), f64_, span)
    }

    /// ToInt32 of the number `h`, as an `i32`.
    pub(super) fn int32_of(&mut self, h: hir::Expr) -> hir::Expr {
        let ty = &self.cx.ty;
        let (f64_, i32_, span) = (ty.f64, ty.i32, h.span);
        if h.ty == i32_ {
            return h;
        }
        if ty.is_int(h.ty) {
            // At most 32 bits (`js_numbers`): ToInt32 of its value is its low 32 bits.
            return self.int_as(h, i32_);
        }
        // A literal (`x | 0`, `y >>> 15`): its ToInt32 is a constant (its 32 bits as a `u32`,
        // reinterpreted).
        if let Some(v) = literal_value(&h) {
            let bits = js_to_int32(v) as u32;
            let u32_ = self.cx.ty.intern(TyKind::Int(IntTy::U32));
            let lit = self.mk(H::Lit(hir::Lit::Int(bits.into())), u32_, span);
            return self.int_as(lit, i32_);
        }
        let x = if h.ty == f64_ {
            h
        } else {
            self.mk(H::Cast(Box::new(h)), f64_, span)
        };
        if let H::Binary {
            op: BinOp::Mul,
            lhs,
            rhs,
        } = x.kind
        {
            return match self.helper_call("__mulToInt32", vec![*lhs, *rhs], i32_, span) {
                Some(call) => call,
                None => self.error_expr(span),
            };
        }
        self.f64_to_int32(x)
    }

    /// ToInt32 of the float `x` (`velt_rt_math_to_int32`).
    fn f64_to_int32(&mut self, x: hir::Expr) -> hir::Expr {
        let (i32_, span) = (self.cx.ty.i32, x.span);
        match self.helper_call("velt_rt_math_to_int32", vec![x], i32_, span) {
            Some(call) => call,
            None => self.error_expr(span),
        }
    }

    /// A call of the prelude function `name` (`None` without a prelude, as in some unit tests).
    fn helper_call(
        &mut self,
        name: &str,
        args: Vec<hir::Expr>,
        ty: TyId,
        span: Span,
    ) -> Option<hir::Expr> {
        let d = self.cx.prelude_fn(name)?;
        Some(self.mk(
            H::Call {
                callee: hir::Callee::Def(d, vec![]),
                args,
            },
            ty,
            span,
        ))
    }
}
