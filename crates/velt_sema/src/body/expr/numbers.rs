//! JS number semantics on fixed-width types (docs/reference/types.md "Numbers").
//!
//! Every integer value is either *declared* (its integer type is written somewhere: an
//! annotation, a parameter, field or return type, a literal suffix, a cast, an API result such
//! as `.length`, or a literal typed by such a context) or *inferred* (an integer literal with no
//! context, a local declared without a type from such a value — `const a = 7`, `let i = 0` —
//! and arithmetic involving one). Both are stored as integers, so counters and indexes keep
//! integer speed; inferred ones behave like JS numbers where that is observable:
//! - `/` is float division unless both operands are declared integers (`a / 2` is `3.5`);
//! - mixed with a float, or used where a float is expected, they convert to it;
//! - `Math.trunc(a / b)` on integers is integer division (one instruction);
//! - next to an integer of another type, they adapt (`let i = 0; i < xs.length`).
//!
//! Integers the standard library hands to user code (`xs.length`, `s.indexOf(t)`, `m.size`,
//! the index of `entries()`) are inferred too: the user wrote no integer type, so they are JS
//! numbers (`xs.length / 2` is `1.5`). Inside `std/` they stay declared.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::{FnCx, Want};
use crate::hir::{self, BinOp, ExprKind as H, Intrinsic, TyId, UnOp};

/// Where an integer value's type comes from (literals adapt to the other operand).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntOrigin {
    Literal,
    Inferred,
    Declared,
}

impl IntOrigin {
    /// Origin of an arithmetic result: inferred if either side is, else declared if either is.
    fn join(self, other: IntOrigin) -> IntOrigin {
        use IntOrigin::*;
        match (self, other) {
            (Inferred, _) | (_, Inferred) => Inferred,
            (Declared, _) | (_, Declared) => Declared,
            _ => Literal,
        }
    }
}

fn arithmetic(op: BinOp) -> bool {
    use BinOp::*;
    matches!(
        op,
        Add | Sub | Mul | Div | Rem | Pow | BitAnd | BitOr | BitXor | Shl | Shr | UShr
    )
}

impl FnCx<'_, '_> {
    /// Origin of the integer value `h` (see the module docs).
    pub(crate) fn int_origin(&self, h: &hir::Expr) -> IntOrigin {
        match &h.kind {
            H::Lit(hir::Lit::Int(_)) => IntOrigin::Literal,
            H::Local(l, _) if self.f.inferred_ints.contains(l) => IntOrigin::Inferred,
            H::Unary {
                op: UnOp::Neg | UnOp::BitNot,
                expr,
            } => self.int_origin(expr),
            H::Binary { op, lhs, rhs } if arithmetic(*op) => {
                self.int_origin(lhs).join(self.int_origin(rhs))
            }
            // A float converted for a bitwise operator is still a JS number (`bitwise_int32`).
            H::Call {
                callee: hir::Callee::Def(d, _),
                ..
            } if self.cx.fn_info(*d).name == "__toInt32" => IntOrigin::Inferred,
            // A conversion the compiler inserted spans exactly its operand and keeps its origin;
            // a written `x as T` also spans `as T`, and declares.
            H::Cast(inner) if inner.span == h.span && self.cx.ty.is_int(inner.ty) => {
                self.int_origin(inner)
            }
            _ if self.is_std_api_value(h) => IntOrigin::Inferred,
            H::Field { base, index, .. } if self.is_inferred_field(base.ty, *index) => {
                IntOrigin::Inferred
            }
            _ => IntOrigin::Declared,
        }
    }

    /// An integer the standard library hands to user code: a length, or the result of a `std/`
    /// function or method (`indexOf`, a `size` getter).
    pub(crate) fn is_std_api_value(&self, h: &hir::Expr) -> bool {
        if self.cx.scopes[self.module].is_std {
            return false;
        }
        match &h.kind {
            H::Call {
                callee: hir::Callee::Intrinsic(Intrinsic::ArrayLen | Intrinsic::StrLen),
                ..
            } => true,
            H::Call {
                callee: hir::Callee::Def(d, _),
                ..
            } => self.cx.scopes[self.cx.fn_info(*d).module].is_std,
            _ => false,
        }
    }

    /// A field declared without a type from an integer literal (`count = 0;`): a JS number.
    fn is_inferred_field(&self, owner: TyId, index: u32) -> bool {
        let Some((d, _)) = self.adt_of(owner) else {
            return false;
        };
        self.cx
            .adt(d)
            .and_then(|a| a.fields.get(index as usize))
            .is_some_and(|f| f.inferred_int)
    }

    /// Operands of two different integer types, at least one of them inferred: the inferred
    /// side adapts. A declared type other than `usize` wins; otherwise both become `i64`, so
    /// `let i = -1; i < xs.length` is `true` as in JS (a length always fits).
    pub(crate) fn mix_ints(&mut self, l: hir::Expr, r: hir::Expr) -> (hir::Expr, hir::Expr) {
        let ty = &self.cx.ty;
        if !(ty.is_int(l.ty) && ty.is_int(r.ty)) || l.ty == r.ty {
            return (l, r);
        }
        let (li, ri) = (self.is_inferred_int(&l), self.is_inferred_int(&r));
        let usize_ = ty.usize;
        let target = match (li, ri) {
            (false, false) => return (l, r),
            (true, false) if r.ty != usize_ => r.ty,
            (false, true) if l.ty != usize_ => l.ty,
            _ => ty.i64,
        };
        (self.int_as(l, target), self.int_as(r, target))
    }

    /// The right operand `v` of `place op= v`, adapted like `place = place op v` would be (#421):
    /// an inferred integer next to a float place converts to it, and two integer types adapt
    /// when either side is inferred (`let total = 0; total += s.length`). The place keeps its
    /// type, so the operand converts to it; a signed value never converts to an unsigned place
    /// (`let k: usize = 0; k -= i` stays an error instead of wrapping a negative result).
    pub(crate) fn compound_operand(&mut self, place: &hir::Expr, v: hir::Expr) -> hir::Expr {
        let (ty, lty) = (&self.cx.ty, place.ty);
        if ty.is_float(lty) && self.is_inferred_int(&v) {
            return self.int_to_float(v, lty);
        }
        let ints = ty.is_int(lty) && ty.is_int(v.ty) && lty != v.ty;
        let signed = |t: TyId| ty.int_ty(t).is_some_and(|i| i.is_signed());
        let wraps = signed(v.ty) && !signed(lty);
        if ints && !wraps && (self.is_inferred_int(&v) || self.is_inferred_int(place)) {
            return self.int_as(v, lty);
        }
        v
    }

    /// The integer `h` converted to integer type `t` (keeping its origin).
    pub(crate) fn int_as(&mut self, h: hir::Expr, t: TyId) -> hir::Expr {
        if h.ty == t {
            return h;
        }
        let span = h.span;
        self.mk(H::Cast(Box::new(h)), t, span)
    }

    /// An integer that behaves like a JS number (not declared with an integer type).
    pub(crate) fn is_inferred_int(&self, h: &hir::Expr) -> bool {
        self.cx.ty.is_int(h.ty) && self.int_origin(h) != IntOrigin::Declared
    }

    /// The initializer of `let x = init` without a type: an inferred `usize` (`xs.length`,
    /// `m.size`) becomes an `i64`, so the local is a JS number that can go negative
    /// (`let n = xs.length; n -= 5` is `-2`). Other integer types were chosen by the program
    /// (a suffix: `const a = 10u8; const b = 1 + a`) and stay.
    pub(crate) fn inferred_local_init(&mut self, init: hir::Expr) -> hir::Expr {
        let (i64_, usize_) = (self.cx.ty.i64, self.cx.ty.usize);
        if init.ty == usize_ && self.int_origin(&init) == IntOrigin::Inferred {
            return self.int_as(init, i64_);
        }
        init
    }

    /// `let x = init` without a type: `x` is an inferred integer when `init` is one.
    pub(crate) fn note_inferred_local(&mut self, local: hir::LocalId, init: &hir::Expr) {
        if self.is_inferred_int(init) {
            self.f.inferred_ints.insert(local);
        }
    }

    /// Integer bindings destructured from a standard library result (`for (const [i, x] of
    /// xs.entries())`) are JS numbers, like the result itself.
    pub(crate) fn note_inferred_bindings(&mut self, p: &hir::Pat, src: &hir::Expr) {
        if !self.is_std_api_value(src) {
            return;
        }
        let mut stack = vec![p];
        while let Some(p) = stack.pop() {
            match &p.kind {
                hir::PatKind::Binding(l, _) if self.cx.ty.is_int(p.ty) => {
                    self.f.inferred_ints.insert(*l);
                }
                hir::PatKind::Tuple(xs) | hir::PatKind::Array { elems: xs, .. } => stack.extend(xs),
                hir::PatKind::Adt { fields } => stack.extend(fields.iter().map(|(_, p)| p)),
                hir::PatKind::Some(x) => stack.push(x),
                _ => {}
            }
        }
    }

    /// The inferred integer `h` as a value of float type `t` (a literal becomes a float literal).
    pub(crate) fn int_to_float(&mut self, h: hir::Expr, t: TyId) -> hir::Expr {
        let span = h.span;
        match h.kind {
            H::Lit(hir::Lit::Int(n)) => self.mk(H::Lit(hir::Lit::Float(n as f64)), t, span),
            _ => self.mk(H::Cast(Box::new(h)), t, span),
        }
    }

    /// Operands of a binary operator: an inferred integer next to a float converts to it.
    pub(crate) fn mix_numbers(&mut self, l: hir::Expr, r: hir::Expr) -> (hir::Expr, hir::Expr) {
        let ty = &self.cx.ty;
        let (lf, rf) = (ty.is_float(l.ty), ty.is_float(r.ty));
        if lf && !rf && self.is_inferred_int(&r) {
            let t = l.ty;
            let r = self.int_to_float(r, t);
            (l, r)
        } else if rf && !lf && self.is_inferred_int(&l) {
            let t = r.ty;
            (self.int_to_float(l, t), r)
        } else {
            (l, r)
        }
    }

    /// Is `l / r` on integers integer division? Both operands declared, or only literals in a
    /// context that expects an integer (`const n: i64 = 7 / 2`).
    fn int_division(&self, l: &hir::Expr, r: &hir::Expr, hint: Option<TyId>) -> bool {
        match self.int_origin(l).join(self.int_origin(r)) {
            IntOrigin::Declared => true,
            IntOrigin::Literal => hint.is_some_and(|t| self.cx.ty.is_int(t)),
            IntOrigin::Inferred => false,
        }
    }

    /// `l / r` of two checked operands of numeric type `t`.
    pub(crate) fn divide(
        &mut self,
        l: hir::Expr,
        r: hir::Expr,
        t: TyId,
        hint: Option<TyId>,
        span: Span,
    ) -> hir::Expr {
        let (l, r, t) = if self.cx.ty.is_int(t) && !self.int_division(&l, &r, hint) {
            let f = self.cx.ty.f64;
            (self.int_to_float(l, f), self.int_to_float(r, f), f)
        } else {
            (l, r, t)
        };
        let kind = H::Binary {
            op: BinOp::Div,
            lhs: Box::new(l),
            rhs: Box::new(r),
        };
        self.mk(kind, t, span)
    }

    /// The hint for a float quotient found where an integer is required.
    pub(crate) fn float_division_note(&self, found: &hir::Expr) -> Option<&'static str> {
        let quotient = matches!(found.kind, H::Binary { op: BinOp::Div, .. });
        (quotient && self.cx.ty.is_float(found.ty)).then_some(
            "`/` gives a float (like JS) unless both operands are declared with integer types; for integer division write `Math.trunc(a / b)`",
        )
    }

    /// `x /= y` on an integer place: allowed only when it stays integer division.
    pub(crate) fn check_int_div_assign(&mut self, place: &hir::Expr, v: &hir::Expr, span: Span) {
        if self.cx.ty.is_int(place.ty) && !self.int_division(place, v, Some(place.ty)) {
            self.cx.error(
                Diagnostic::error(
                    "`/=` would store a float in an integer variable",
                    span,
                )
                .with_note(
                    "`/` gives a float (like JS) unless both operands are declared with integer types",
                )
                .with_note("for integer division write `x = Math.trunc(x / y)`, or declare the variable as a float (`let x = 0.0`)"),
            );
        }
    }

    /// `Math.trunc(a / b)` (the prelude's `Math`): integer division when both operands are
    /// integers, else `trunc` of the float quotient. `None` if the call is anything else.
    pub(crate) fn math_trunc_div(
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
        let (ast::ExprKind::Ident(m), [arg]) = (&object.kind, args) else {
            return None;
        };
        let mut arg = arg;
        while let ast::ExprKind::Paren(x) = &arg.kind {
            arg = x;
        }
        let ast::ExprKind::Binary {
            op: ast::BinaryOp::Div,
            lhs,
            rhs,
        } = &arg.kind
        else {
            return None;
        };
        if m.name != "Math" || prop.name != "trunc" || self.is_local_name("Math") {
            return None;
        }
        let math = self.cx.prelude_adt("Math")?;
        match self.cx.lookup_item_at(self.module, "Math", m.span) {
            Some(crate::ctx::Item::Def(d)) if d == math => {}
            _ => return None,
        }
        let (l, r) = self.operands(lhs, rhs, None, Want::Borrow);
        let (l, r) = self.mix_numbers(l, r);
        let (l, r) = self.mix_ints(l, r);
        let Some(t) = self.check_operands(ast::BinaryOp::Div, l.ty, &r, arg.span) else {
            return Some(self.error_expr(span));
        };
        if self.cx.ty.is_int(t) {
            let kind = H::Binary {
                op: BinOp::Div,
                lhs: Box::new(l),
                rhs: Box::new(r),
            };
            return Some(self.mk(kind, t, span));
        }
        let q = self.divide(l, r, t, None, arg.span);
        let ty = q.ty;
        Some(self.intrinsic(Intrinsic::Trunc, vec![q], ty, span))
    }
}

impl FnCx<'_, '_> {
    /// Operands of a bitwise operator: a float one is converted like JS's `ToInt32`
    /// (`(a / 13) | 0`, the JS truncation idiom), through the prelude's `__toInt32`, and is then an
    /// inferred `i64`. Integer operands are left alone.
    pub(super) fn bitwise_int32(
        &mut self,
        op: ast::BinaryOp,
        l: hir::Expr,
        r: hir::Expr,
    ) -> (hir::Expr, hir::Expr) {
        use ast::BinaryOp as B;
        let bitwise = matches!(
            op,
            B::BitAnd | B::BitOr | B::BitXor | B::Shl | B::Shr | B::UShr
        );
        if !bitwise || !(self.cx.ty.is_float(l.ty) || self.cx.ty.is_float(r.ty)) {
            return (l, r);
        }
        (self.as_int32(l), self.as_int32(r))
    }

    /// A float index (`xs[i]` with `i: number`, `xs[Math.floor(n / 2)]`) as a `usize`, through
    /// the prelude's `__floatIndex`: a whole number indexes as usual, anything else panics (in
    /// JS it reads `undefined`).
    pub(super) fn float_index(&mut self, h: hir::Expr) -> hir::Expr {
        let Some(crate::ctx::Item::Def(d)) = self.cx.prelude.get("__floatIndex").copied() else {
            return h;
        };
        let span = h.span;
        let f64_ = self.cx.ty.f64;
        let arg = if h.ty == f64_ {
            h
        } else {
            self.mk(H::Cast(Box::new(h)), f64_, span)
        };
        self.mk(
            H::Call {
                callee: hir::Callee::Def(d, vec![]),
                args: vec![arg],
            },
            self.cx.ty.usize,
            span,
        )
    }

    /// A float as JS's ToInt32 of it (an inferred `i64`); other values unchanged.
    pub(super) fn as_int32(&mut self, h: hir::Expr) -> hir::Expr {
        if !self.cx.ty.is_float(h.ty) {
            return h;
        }
        let Some(crate::ctx::Item::Def(d)) = self.cx.prelude.get("__toInt32").copied() else {
            return h;
        };
        let span = h.span;
        let (f64_, i64_) = (self.cx.ty.f64, self.cx.ty.i64);
        let arg = if h.ty == f64_ {
            h
        } else {
            self.mk(H::Cast(Box::new(h)), f64_, span)
        };
        self.mk(
            H::Call {
                callee: hir::Callee::Def(d, vec![]),
                args: vec![arg],
            },
            i64_,
            span,
        )
    }
}
