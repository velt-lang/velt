//! A number operand of a `velt/math` `BigInt` operator (design operators.md, #2096): `b + 3`,
//! `3 * b` and `b += 3` are errors, as in JavaScript (`TypeError: Cannot mix BigInt and other
//! types`) and TypeScript (TS2365), with the fix in the note: a `BigInt` for the other operand,
//! or the named twin that takes an `i64` (`b.addInt(3)`). Shift counts are `i64`s.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir::{LitValue, TyId, TyKind};

/// `op`'s symbol and its method's name on `BigInt` (`addInt` is the twin taking an `i64`).
fn arithmetic(op: ast::BinaryOp) -> Option<(&'static str, &'static str)> {
    use ast::BinaryOp as B;
    Some(match op {
        B::Add => ("+", "add"),
        B::Sub => ("-", "sub"),
        B::Mul => ("*", "mul"),
        B::Div => ("/", "div"),
        B::Rem => ("%", "rem"),
        _ => return None,
    })
}

impl FnCx<'_, '_> {
    /// Is `t` `velt/math`'s `BigInt`?
    fn is_bigint(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Adt(d, _) => self
                .cx
                .adt(*d)
                .is_some_and(|a| a.qual_name == velt_common::std_item!(math_bigint, "BigInt")),
            _ => false,
        }
    }

    /// Is `t` a number type or a number literal type?
    fn is_number_ty(&self, t: TyId) -> bool {
        match self.cx.ty.kind(t) {
            TyKind::Int(_) | TyKind::Float(_) => true,
            TyKind::Literal(v) => matches!(v, LitValue::Int(..) | LitValue::Float(..)),
            _ => false,
        }
    }

    /// `l op r` (or `l op= r`, `assign`) mixing a `BigInt` and a number: reported, `true`.
    pub(super) fn bigint_mixed(
        &mut self,
        op: ast::BinaryOp,
        (l, r): (TyId, TyId),
        assign: bool,
        span: Span,
    ) -> bool {
        let Some((sym, method)) = arithmetic(op) else {
            return false;
        };
        let left = self.is_bigint(l) && self.is_number_ty(r);
        if !left && !(self.is_bigint(r) && self.is_number_ty(l)) {
            return false;
        }
        let shown = self.cx.display(if left { r } else { l });
        let sym = if assign {
            format!("{sym}=")
        } else {
            sym.to_string()
        };
        let (fix, fast) = match (left, assign) {
            (true, false) => (
                format!("b {sym} new BigInt(3)"),
                format!("b.{method}Int(3)"),
            ),
            (true, true) if method != "rem" => (
                format!("b {sym} new BigInt(3)"),
                format!("b.{method}IntAssign(3)"),
            ),
            (true, true) => (format!("b {sym} new BigInt(3)"), String::new()),
            // `+` and `*` commute: the twin on the BigInt.
            (false, _) if matches!(method, "add" | "mul") => (
                format!("new BigInt(3) {sym} b"),
                format!("b.{method}Int(3)"),
            ),
            (false, _) => (format!("new BigInt(3) {sym} b"), String::new()),
        };
        let d = Diagnostic::error(
            format!("BigInt arithmetic takes BigInt operands, not a `{shown}`"),
            span,
        );
        let d = match fast.is_empty() {
            true => d.with_note(format!("write `{fix}`")),
            false => d.with_note(format!("write `{fix}`, or `{fast}` for the fast form")),
        };
        self.cx.error(d.with_note(
            "JavaScript throws on mixing BigInt and other types, and TypeScript rejects it",
        ));
        true
    }
}
