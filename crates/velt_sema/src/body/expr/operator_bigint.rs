//! A number operand of a `velt/math` `BigInt` operator (design operators.md, #2096): `b + 3`,
//! `3 * b` and `b += 3` are errors, as in JavaScript (`TypeError: Cannot mix BigInt and other
//! types`) and TypeScript (TS2365), with the fix in the note: a `BigInt` for the other operand,
//! or the named twin that takes an `i64` (`b.addInt(3)`). Shift counts are `i64`s.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use crate::body::switch::cases::source_text;
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

/// Does `source_text` write `e` as it is: a name, `this`, a field path or a number literal?
/// (Anything else it calls "the value", which a variable could be named too.)
fn quotable(e: &ast::Expr) -> bool {
    match &e.kind {
        ast::ExprKind::Paren(inner) => quotable(inner),
        ast::ExprKind::Ident(_) | ast::ExprKind::This => true,
        ast::ExprKind::Member { object, .. } => quotable(object),
        ast::ExprKind::Lit(ast::Lit::Int { .. } | ast::Lit::Float { .. }) => true,
        ast::ExprKind::Unary {
            op: ast::UnaryOp::Neg,
            expr,
        } => matches!(
            &expr.kind,
            ast::ExprKind::Lit(ast::Lit::Int { .. } | ast::Lit::Float { .. })
        ),
        _ => false,
    }
}

impl FnCx<'_, '_> {
    /// Is `t` `velt/math`'s `BigInt`?
    pub(super) fn is_bigint(&self, t: TyId) -> bool {
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
        (lhs, rhs): (&ast::Expr, &ast::Expr),
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
        // The operands as written (`w`, `k`, `3`), or `b` and `k` for anything longer.
        let (big_e, num_e) = if left { (lhs, rhs) } else { (rhs, lhs) };
        let text = |e: &ast::Expr, other: &str| match quotable(e) {
            true => source_text(e),
            false => other.to_string(),
        };
        let (b, k) = (text(big_e, "b"), text(num_e, "k"));
        let (fix, fast) = match (left, assign) {
            (true, false) => (
                format!("{b} {sym} new BigInt({k})"),
                format!("{b}.{method}Int({k})"),
            ),
            (true, true) if method != "rem" => (
                format!("{b} {sym} new BigInt({k})"),
                format!("{b}.{method}IntAssign({k})"),
            ),
            (true, true) => (format!("{b} {sym} new BigInt({k})"), String::new()),
            // `+` and `*` commute: the twin on the BigInt.
            (false, _) if matches!(method, "add" | "mul") => (
                format!("new BigInt({k}) {sym} {b}"),
                format!("{b}.{method}Int({k})"),
            ),
            (false, _) => (format!("new BigInt({k}) {sym} {b}"), String::new()),
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
