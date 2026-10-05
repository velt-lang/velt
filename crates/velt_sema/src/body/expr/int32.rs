//! JavaScript's 32-bit integer operators on numbers (issue #521; design #525, step 1).
//!
//! When their operands are numbers (floats, inferred integers (`numbers.rs`), or literals where
//! no integer type is expected), `| & ^ << >> ~` apply ToInt32 to the operands and `>>>`
//! applies ToUint32. They compile to 32-bit integer operations:
//! - an integer operand contributes its low 32 bits (`as i32`), which is ToInt32 of its value;
//! - a float operand goes through the runtime's ToInt32 (`velt_rt_math_to_int32`), which the
//!   backends emit inline (one conversion on the common path);
//! - a product inside an operand is rounded the way JS's double multiply rounds it past 2^53:
//!   `(y * k) | 0` through the prelude's `__mulToInt32`, and other arithmetic in an operand
//!   (`(a * b - 1) | 0`, `(x + 1) | 0`) is computed as doubles, which `velt_opt`'s `numrep`
//!   turns back into integer operations where it proves them exact;
//! - shift counts are taken modulo 32, and the result is an inferred `i64`: sign-extended, or
//!   zero-extended for `>>>`.
//!
//! `Math.imul` and `Math.clz32` compile the same way, to one multiply or count. Operands of a
//! declared integer type keep their own (Rust) semantics: `n >>> 3` with `n: i64` is a 64-bit
//! shift.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::numbers::IntOrigin;
use crate::body::{FnCx, LocalKind, Want};
use crate::hir::{self, BinOp, ExprKind as H, IntTy, TyId, TyKind, UnOp, UseMode};

/// Prelude functions whose integer result is a JS number (`numbers.rs` `int_origin`).
pub(crate) const INT32_HELPERS: [&str; 4] = [
    "velt_rt_math_to_int32",
    "velt_rt_math_clz32",
    "__mulToInt32",
    "__intAddToInt32",
];

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

/// Is `e` (in parentheses or not) a bitwise operation: `a | b`, `a >>> b`, `~a` and the like?
fn is_bitwise_expr(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(inner) => is_bitwise_expr(inner),
        ast::ExprKind::Binary { op, .. } => bitwise_op(*op).is_some(),
        ast::ExprKind::Unary {
            op: ast::UnaryOp::BitNot,
            ..
        } => true,
        _ => false,
    }
}

impl FnCx<'_, '_> {
    /// A JS number for the int32 operators: a float, or an integer not declared with a type.
    fn js_number(&self, h: &hir::Expr) -> bool {
        let ty = &self.cx.ty;
        ty.is_float(h.ty) || (ty.is_int(h.ty) && self.int_origin(h) != IntOrigin::Declared)
    }

    fn is_int_literal(&self, h: &hir::Expr) -> bool {
        self.cx.ty.is_int(h.ty) && self.int_origin(h) == IntOrigin::Literal
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
    /// numbers. Two literals where an integer type is expected (`const m: u64 = 1 << 40`) stay a
    /// constant of that type.
    pub(super) fn js_bitwise_applies(
        &self,
        op: ast::BinaryOp,
        l: &hir::Expr,
        r: &hir::Expr,
        hint: Option<TyId>,
    ) -> Option<BinOp> {
        let bop = bitwise_op(op)?;
        let typed_constant = self.is_int_literal(l)
            && self.is_int_literal(r)
            && hint.is_some_and(|t| self.cx.ty.is_int(t));
        (!typed_constant && self.js_number(l) && self.js_number(r)).then_some(bop)
    }

    /// `~x` takes JS's 32-bit semantics: `x` is a number (see `js_bitwise_applies`).
    pub(super) fn js_bitnot_applies(&self, x: &hir::Expr, hint: Option<TyId>) -> bool {
        let typed_constant = self.is_int_literal(x) && hint.is_some();
        !typed_constant && self.js_number(x)
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
        if bitwise_op(op).is_some() && self.js_number(place) && self.js_number(&value) {
            Ok(value)
        } else {
            Err(value)
        }
    }

    /// The value of `place op= value` (`js_bitwise_operands` holds; `cur` reads the place):
    /// `place op value` with JS semantics, converted back to the place's type (`x |= 0` on a
    /// float stores ToInt32 of it).
    pub(super) fn js_bitwise_assign(
        &mut self,
        op: ast::BinaryOp,
        place: &hir::Expr,
        cur: hir::Expr,
        value: hir::Expr,
        span: Span,
    ) -> hir::Expr {
        let bop = bitwise_op(op).expect("ICE: js_bitwise_assign on a non-bitwise operator");
        let v = self.int32_binary(bop, cur, value, span);
        let lty = place.ty;
        if self.cx.ty.is_float(lty) {
            self.mk(H::Cast(Box::new(v)), lty, span)
        } else {
            self.int_as(v, lty)
        }
    }

    /// The expected type of the value assigned to `place`: its type, except that a bitwise
    /// expression assigned to a JS number held as an integer (`let a = 0; a = 1 << 31`) is
    /// checked without one, so two literals take JS's 32-bit semantics rather than a typed
    /// constant's (an integer type written in the source keeps them).
    pub(super) fn value_hint(&self, place: &hir::Expr, value: &ast::Expr) -> Option<TyId> {
        let inferred_int = self.cx.ty.is_int(place.ty) && self.js_number(place);
        (!(inferred_int && is_bitwise_expr(value))).then_some(place.ty)
    }

    /// Binds each index of `place` that is not a literal (`xs[next()]`) to a temporary (appended
    /// to `stmts` as `let`s), so that a read-modify-write of the place evaluates it once.
    pub(super) fn hoist_indices(&mut self, place: &mut hir::Expr, stmts: &mut Vec<hir::Stmt>) {
        match &mut place.kind {
            H::Field { base, .. } => self.hoist_indices(base, stmts),
            H::Index { base, index, .. } => {
                self.hoist_indices(base, stmts);
                if matches!(index.kind, H::Lit(_)) {
                    return;
                }
                let (ty, span) = (index.ty, index.span);
                let tmp = self.new_local("<index>", ty, false, span, LocalKind::Temp);
                let init = std::mem::replace(
                    &mut **index,
                    self.mk(H::Local(tmp, UseMode::Copy), ty, span),
                );
                let kind = hir::StmtKind::Let {
                    local: tmp,
                    init: Some(init),
                };
                stmts.push(hir::Stmt { kind, span });
            }
            _ => {}
        }
    }

    /// `e` after the statements `stmts` (as a block when there are any).
    pub(super) fn with_temps(&mut self, mut stmts: Vec<hir::Stmt>, e: hir::Expr) -> hir::Expr {
        if stmts.is_empty() {
            return e;
        }
        let (ty, span) = (e.ty, e.span);
        stmts.push(hir::Stmt {
            kind: hir::StmtKind::Expr(e),
            span,
        });
        let block = hir::Block {
            stmts,
            value: None,
            span,
        };
        self.mk(H::Block(block), ty, span)
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
            if !self.cx.ty.is_numeric(v.ty) {
                if !self.cx.ty.is_bottom(v.ty) {
                    let found = self.cx.display(v.ty);
                    self.cx.error(
                        Diagnostic::error("mismatched types", a.span)
                            .with_note(format!("expected number, found {found}")),
                    );
                }
                return Some(self.error_expr(span));
            }
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

    /// A 32-bit result as the inferred `i64` it is in JS (the cast spans exactly its operand,
    /// so it keeps the operand's origin).
    pub(super) fn widen32(&mut self, v: hir::Expr) -> hir::Expr {
        let (i64_, span) = (self.cx.ty.i64, v.span);
        self.mk(H::Cast(Box::new(v)), i64_, span)
    }

    /// ToInt32 of the number `h`, as an `i32`.
    pub(super) fn int32_of(&mut self, h: hir::Expr) -> hir::Expr {
        let ty = &self.cx.ty;
        let (f64_, i32_, span) = (ty.f64, ty.i32, h.span);
        if h.ty == f64_ && self.int_plus_float(&h) {
            return self.int_sum_to_int32(h);
        }
        if ty.is_float(h.ty) {
            let x = if h.ty == f64_ {
                h
            } else {
                self.mk(H::Cast(Box::new(h)), f64_, span)
            };
            return self.f64_to_int32(x);
        }
        if h.ty == i32_ {
            return h;
        }
        if let Some(p) = self.js_product(&h) {
            let (a, b) = self.split_binary(h);
            if !self.has_js_arith(&a) && !self.has_js_arith(&b) {
                return match self.helper_call("__mulToInt32", vec![a, b], i32_, p) {
                    Some(call) => call,
                    None => self.error_expr(p),
                };
            }
            let kind = H::Binary {
                op: BinOp::Mul,
                lhs: Box::new(a),
                rhs: Box::new(b),
            };
            let h = self.mk(kind, self.cx.ty.i64, p);
            let x = self.js_f64(h);
            return self.f64_to_int32(x);
        }
        if self.has_js_arith(&h) {
            let x = self.js_f64(h);
            return self.f64_to_int32(x);
        }
        self.int_as(h, i32_)
    }

    /// ToInt32 of the float `x` (`velt_rt_math_to_int32`).
    fn f64_to_int32(&mut self, x: hir::Expr) -> hir::Expr {
        let (i32_, span) = (self.cx.ty.i32, x.span);
        match self.helper_call("velt_rt_math_to_int32", vec![x], i32_, span) {
            Some(call) => call,
            None => self.error_expr(span),
        }
    }

    /// Is `h` the sum or difference of an inferred integer (converted to `f64` next to a float,
    /// `numbers.rs` `mix_numbers`) and a float, as in `(y + i) | 0` with `i: number`?
    fn int_plus_float(&self, h: &hir::Expr) -> bool {
        let H::Binary {
            op: BinOp::Add | BinOp::Sub,
            lhs,
            rhs,
        } = &h.kind
        else {
            return false;
        };
        self.converted_int(lhs).is_some() != self.converted_int(rhs).is_some()
    }

    /// The `i64` inside `h` when `h` is an inferred integer converted to `f64`.
    fn converted_int<'e>(&self, h: &'e hir::Expr) -> Option<&'e hir::Expr> {
        match &h.kind {
            H::Cast(inner) if inner.ty == self.cx.ty.i64 && self.js_i64(inner) => Some(inner),
            _ => None,
        }
    }

    /// ToInt32 of `a ± x` (`int_plus_float`) through the prelude's `__intAddToInt32(a, x)`, which
    /// adds as integers when `x` is a whole number of at most 2^52 (exactly what the double add
    /// computes then) and as doubles otherwise. `a - x` passes `-x`, and `x - a` passes `-a`.
    fn int_sum_to_int32(&mut self, h: hir::Expr) -> hir::Expr {
        let (f64_, i64_, i32_, span) = (self.cx.ty.f64, self.cx.ty.i64, self.cx.ty.i32, h.span);
        let sub = matches!(h.kind, H::Binary { op: BinOp::Sub, .. });
        let int_left =
            matches!(&h.kind, H::Binary { lhs, .. } if self.converted_int(lhs).is_some());
        let (l, r) = self.split_binary(h);
        let (int_side, float_side) = if int_left { (l, r) } else { (r, l) };
        let H::Cast(a) = int_side.kind else {
            return self.error_expr(span);
        };
        let neg = |s: &Self, e: hir::Expr, t: TyId| {
            let sp = e.span;
            s.mk(
                H::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(e),
                },
                t,
                sp,
            )
        };
        if self.has_js_arith(&a) {
            let a = self.js_f64(*a);
            let (lhs, rhs) = if int_left {
                (a, float_side)
            } else {
                (float_side, a)
            };
            let op = if sub { BinOp::Sub } else { BinOp::Add };
            let kind = H::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
            let x = self.mk(kind, f64_, span);
            return self.f64_to_int32(x);
        }
        let a = *a;
        let (a, x) = match (sub, int_left) {
            (true, true) => (a, neg(self, float_side, f64_)),
            (true, false) => (neg(self, a, i64_), float_side),
            _ => (a, float_side),
        };
        match self.helper_call("__intAddToInt32", vec![a, x], i32_, span) {
            Some(call) => call,
            None => self.error_expr(span),
        }
    }

    /// The span of `h` if it is a product of two inferred `i64` numbers.
    fn js_product(&self, h: &hir::Expr) -> Option<Span> {
        let is_mul = matches!(&h.kind, H::Binary { op: BinOp::Mul, .. });
        (is_mul && self.js_i64(h)).then_some(h.span)
    }

    fn js_i64(&self, h: &hir::Expr) -> bool {
        h.ty == self.cx.ty.i64 && self.int_origin(h) != IntOrigin::Declared
    }

    /// Is `h` inferred-integer arithmetic (a sum, difference or product, or a negation of one)
    /// that JS computes with doubles, rounding results past 2^53?
    fn has_js_arith(&self, h: &hir::Expr) -> bool {
        if !self.js_i64(h) {
            return false;
        }
        match &h.kind {
            H::Binary {
                op: BinOp::Add | BinOp::Sub | BinOp::Mul,
                ..
            } => true,
            H::Unary {
                op: UnOp::Neg,
                expr,
            } => self.has_js_arith(expr),
            _ => false,
        }
    }

    /// The JS value of the inferred-integer arithmetic `h` as a double: its sums, differences
    /// and products computed as doubles, the way JS rounds them (and never saturating), from
    /// its other parts converted. `numrep` turns the operations it proves exact back into
    /// integer ones (#561: `(x + 1) | 0` with `x = 2^53` adds as doubles).
    fn js_f64(&mut self, h: hir::Expr) -> hir::Expr {
        let (f64_, span) = (self.cx.ty.f64, h.span);
        if !self.has_js_arith(&h) {
            return self.mk(H::Cast(Box::new(h)), f64_, span);
        }
        let kind = match h.kind {
            H::Unary { op, expr } => H::Unary {
                op,
                expr: Box::new(self.js_f64(*expr)),
            },
            H::Binary { op, lhs, rhs } => H::Binary {
                op,
                lhs: Box::new(self.js_f64(*lhs)),
                rhs: Box::new(self.js_f64(*rhs)),
            },
            _ => panic!("ICE: js_f64 on a leaf with arithmetic"),
        };
        self.mk(kind, f64_, span)
    }

    /// The operands of the binary expression `h` (an ICE otherwise; callers matched it).
    fn split_binary(&self, h: hir::Expr) -> (hir::Expr, hir::Expr) {
        match h.kind {
            H::Binary { lhs, rhs, .. } => (*lhs, *rhs),
            _ => panic!("ICE: split_binary on a non-binary expression"),
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
