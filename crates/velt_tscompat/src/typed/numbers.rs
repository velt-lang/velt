//! Rules on numbers: `/` on integer types (`int-division`), subtraction that wraps on unsigned
//! integers (`unsigned-arith`), and sorting numbers without a comparator (`default-sort`).

use velt_common::Span;
use velt_sema::ide::{LiteralKind, TypeView};
use velt_syntax::ast::{self, BinaryOp, ExprKind as E, UpdateOp};

use super::Typed;
use crate::{Fix, Severity};

/// `a / b`: Velt divides integers when both operands have integer types; `a - b` on unsigned
/// integers.
pub(super) fn binary(e: &ast::Expr, op: BinaryOp, lhs: &ast::Expr, rhs: &ast::Expr, t: &mut Typed) {
    match op {
        BinaryOp::Div if !t.truncated.contains(&(e.span.lo, e.span.hi)) => {
            if matches!(t.view_of(e), TypeView::Int(_)) {
                let fix = division_fix(e, lhs, rhs, t);
                int_division(e.span, fix, t);
            }
        }
        BinaryOp::Sub => unsigned(e.span, &t.view_of(e), "-", t),
        _ => {}
    }
}

/// `Math.trunc(a / b)` where both operands have integer types (Velt's result, in both
/// languages); `(a as number) / 2` where `a`'s type is made of integer literal types (`1 | 3`)
/// and `b` is a number literal (JavaScript's result: Velt's `Math.trunc` keeps the literal
/// types there, and rejects the quotient). Otherwise none: an `as number` on a declared
/// integer would need the other operand converted too, and the context may need an integer.
fn division_fix(e: &ast::Expr, lhs: &ast::Expr, rhs: &ast::Expr, t: &Typed) -> Option<Fix> {
    let is_int = |x: &ast::Expr| matches!(t.view_of(x), TypeView::Int(_));
    if is_int(lhs) && is_int(rhs) {
        return Some(Fix {
            span: e.span,
            replacement: format!("Math.trunc({})", t.cx.text(e.span)),
            title: "truncate with `Math.trunc` in both languages".into(),
        });
    }
    if !matches!(rhs.kind, E::Lit(ast::Lit::Int { suffix: None, .. })) {
        return None;
    }
    let lhs_text = t.cx.text(lhs.span);
    Some(Fix {
        span: lhs.span,
        replacement: format!("({lhs_text} as number)"),
        title: "divide as numbers: `as number`".into(),
    })
}

/// `x /= y` and `x -= y`.
pub(super) fn assign(
    e: &ast::Expr,
    op: Option<BinaryOp>,
    target: &ast::Expr,
    value: &ast::Expr,
    t: &mut Typed,
) {
    let view = t.place_view(target);
    match op {
        Some(BinaryOp::Div) if matches!(view, TypeView::Int(_)) => {
            let value_text = match &value.kind {
                E::Lit(_) | E::Ident(_) | E::Member { .. } | E::Call { .. } | E::Paren(_) => {
                    t.cx.text(value.span).to_string()
                }
                _ => format!("({})", t.cx.text(value.span)),
            };
            let fix = match &target.kind {
                E::Ident(id) => Some(Fix {
                    span: e.span,
                    replacement: format!("{0} = Math.trunc({0} / {value_text})", id.name),
                    title: "truncate with `Math.trunc` in both languages".into(),
                }),
                _ => None,
            };
            int_division(e.span, fix, t);
        }
        Some(BinaryOp::Sub) => unsigned(e.span, &view, "-=", t),
        _ => {}
    }
}

/// `x--` / `--x`.
pub(super) fn update(e: &ast::Expr, op: UpdateOp, target: &ast::Expr, t: &mut Typed) {
    if op == UpdateOp::Dec {
        unsigned(e.span, &t.place_view(target), "--", t);
    }
}

fn int_division(span: Span, fix: Option<Fix>, t: &mut Typed) {
    t.cx.report(
        "int-division",
        Severity::Error,
        span,
        "`/` divides integers here: Velt truncates the quotient, JavaScript doesn't".into(),
        &[
            "both operands have integer types, so Velt divides integers and truncates toward \
             zero (`7 / 2` is `3`); TypeScript has only `number`, and JavaScript gives `3.5`",
            "write `Math.trunc(a / b)`, which truncates in both, or give the operands the type \
             `number` for the fraction",
        ],
        fix,
    );
}

fn unsigned(span: Span, view: &TypeView, op: &str, t: &mut Typed) {
    let TypeView::Int(int) = view else { return };
    if int.is_signed() {
        return;
    }
    t.cx.report(
        "unsigned-arith",
        Severity::Warning,
        span,
        format!("`{op}` on an unsigned integer wraps around below zero in Velt"),
        &[
            "the value is unsigned (a length, a size, or a `u` type), so where JavaScript \
             gives a negative number (`[].length - 1` is `-1`) Velt wraps to a huge one",
            "compare before subtracting (`i < xs.length` rather than `i <= xs.length - 1`), \
             or keep the arithmetic away from zero",
        ],
        None,
    );
}

/// `Math.trunc(a / b)` (its division truncates in both languages) and `xs.sort()`.
pub(super) fn call(e: &ast::Expr, callee: &ast::Expr, args: &[ast::Expr], t: &mut Typed) {
    let E::Member { object, prop, .. } = &callee.kind else {
        return;
    };
    match (prop.name.as_str(), args) {
        ("trunc", [arg]) if is_global(object, "Math", t) => {
            let mut inner = arg;
            while let E::Paren(x) = &inner.kind {
                inner = x;
            }
            t.truncated.insert((inner.span.lo, inner.span.hi));
        }
        ("sort" | "toSorted", []) => default_sort(e, object, prop, t),
        _ => {}
    }
}

/// `e` is the identifier `name` with no definition in user code (the prelude's or a builtin).
pub(super) fn is_global(e: &ast::Expr, name: &str, t: &Typed) -> bool {
    match &e.kind {
        E::Ident(id) if id.name == name => t.def(id.span).is_none_or(|d| t.is_std(&d)),
        _ => false,
    }
}

fn default_sort(call: &ast::Expr, object: &ast::Expr, prop: &ast::Ident, t: &mut Typed) {
    let TypeView::Array(elem) = t.view_of(object) else {
        return;
    };
    let fixable = match t.view(&elem) {
        TypeView::Int(i) => i.is_signed(),
        TypeView::Float(_) | TypeView::Literal(LiteralKind::Int | LiteralKind::Float) => true,
        _ => return,
    };
    let message = format!(
        "`{}()` without a comparator sorts numbers by value in Velt, as text in JavaScript",
        prop.name
    );
    let notes = [
        "JavaScript's default order compares elements as strings, so `[10, 9, 1].sort()` is \
         `[1, 10, 9]`; Velt sorts numbers by value",
        "pass a comparator: `(a, b) => a - b`",
    ];
    // The call ends with `()`: the comparator goes between them.
    let at = call.span.hi.saturating_sub(1);
    let fix = fixable.then(|| Fix {
        span: Span::new(call.span.file, at, at),
        replacement: "(a, b) => a - b".into(),
        title: "sort by value: `(a, b) => a - b`".into(),
    });
    t.cx.report(
        "default-sort",
        Severity::Error,
        prop.span,
        message,
        &notes,
        fix,
    );
}
