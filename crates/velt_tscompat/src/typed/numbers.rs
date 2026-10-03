//! Rules on numbers: `/` on integer types (`int-division`), subtraction that wraps on unsigned
//! integers (`unsigned-arith`), and sorting numbers without a comparator (`default-sort`).

use velt_common::Span;
use velt_sema::ide::{LiteralKind, TypeView};
use velt_syntax::ast::{self, BinaryOp, ExprKind as E, UpdateOp};

use super::Typed;
use crate::rules::types::INT_TYPES;
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
/// languages); `(a as number) / 2` where `a`'s type is made of integer literal types (`1 | 3`),
/// `b` is a number literal and the quotient goes nowhere that declares an integer
/// ([`int_position`]; JavaScript's result: Velt's `Math.trunc` keeps the literal types there,
/// and rejects the quotient). Otherwise none: an `as number` on a declared integer would need
/// the other operand converted too, and a fraction doesn't fit where an integer is declared.
fn division_fix(e: &ast::Expr, lhs: &ast::Expr, rhs: &ast::Expr, t: &Typed) -> Option<Fix> {
    let is_int = |x: &ast::Expr| matches!(t.view_of(x), TypeView::Int(_));
    if is_int(lhs) && is_int(rhs) {
        return Some(Fix {
            span: e.span,
            replacement: format!("Math.trunc({})", t.cx.text(e.span)),
            title: "truncate with `Math.trunc` in both languages".into(),
        });
    }
    if !matches!(rhs.kind, E::Lit(ast::Lit::Int { suffix: None, .. }))
        || !literal_int(&t.view_of(lhs), t)
        || t.int_positions.contains(&(e.span.lo, e.span.hi))
    {
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

/// Whether `view` is an integer literal type (`3`) or a union of them (`1 | 3`).
fn literal_int(view: &TypeView, t: &Typed) -> bool {
    match view {
        TypeView::Literal(LiteralKind::Int) => true,
        TypeView::Union(members) => members.iter().all(|m| literal_int(&t.view(m), t)),
        _ => false,
    }
}

/// Whether `view` is an integer type: a fixed-width integer or [`literal_int`].
pub(super) fn int_view(view: &TypeView, t: &Typed) -> bool {
    matches!(view, TypeView::Int(_)) || literal_int(view, t)
}

/// Whether the written type `ty` is an integer type (`i64`, `3`, `1 | 3`, `i64 | null`).
pub(super) fn int_type_expr(ty: &ast::TypeExpr) -> bool {
    use ast::TypeExprKind as T;
    match &ty.kind {
        T::Named { path, args } => match (path.as_slice(), args.is_empty()) {
            ([name], true) => INT_TYPES.contains(&name.name.as_str()),
            _ => false,
        },
        T::Literal(lit) => matches!(lit.lit, ast::Lit::Int { .. }),
        T::Union(members) => {
            members.iter().any(|m| !matches!(m.kind, T::Null))
                && members
                    .iter()
                    .all(|m| matches!(m.kind, T::Null) || int_type_expr(m))
        }
        _ => false,
    }
}

/// `e` goes where an integer is declared (a return value, a variable, an argument, a field),
/// or into arithmetic that does: an `as number` fix in it would hand a fraction there.
pub(super) fn int_position(e: &ast::Expr, t: &mut Typed) {
    t.int_positions.insert((e.span.lo, e.span.hi));
    match &e.kind {
        E::Paren(x) | E::Unary { expr: x, .. } => int_position(x, t),
        E::Cond { then, els, .. } => {
            int_position(then, t);
            int_position(els, t);
        }
        E::Binary { op, lhs, rhs } if arithmetic(*op) => {
            int_position(lhs, t);
            int_position(rhs, t);
        }
        _ => {}
    }
}

fn arithmetic(op: BinaryOp) -> bool {
    use BinaryOp as B;
    matches!(
        op,
        B::Add
            | B::Sub
            | B::Mul
            | B::Div
            | B::Rem
            | B::Pow
            | B::BitAnd
            | B::BitOr
            | B::BitXor
            | B::Shl
            | B::Shr
            | B::UShr
    )
}

/// An operand of arithmetic or a comparison whose other operand has a declared integer type
/// (`i64`) must stay an integer.
pub(super) fn operand_positions(op: BinaryOp, lhs: &ast::Expr, rhs: &ast::Expr, t: &mut Typed) {
    use BinaryOp as B;
    let compares = matches!(op, B::Eq | B::NotEq | B::Lt | B::LtEq | B::Gt | B::GtEq);
    if !arithmetic(op) && !compares {
        return;
    }
    if matches!(t.view_of(rhs), TypeView::Int(_)) {
        int_position(lhs, t);
    }
    if matches!(t.view_of(lhs), TypeView::Int(_)) {
        int_position(rhs, t);
    }
}

/// The variable `pattern` declares has an integer type.
pub(super) fn int_local(pattern: &ast::Pattern, t: &Typed) -> bool {
    let ast::PatternKind::Ident(id) = &pattern.kind else {
        return false;
    };
    match t.program.analysis.type_of(id.span) {
        Some(ty) => int_view(&t.view(&ty), t),
        None => false,
    }
}

/// `{ f: v }` where the field `f` has an integer type.
pub(super) fn object(e: &ast::Expr, props: &[ast::ObjectProp], t: &mut Typed) {
    let Some(ty) = t.type_of(e) else { return };
    let fields = t.program.analysis.fields(&ty);
    for prop in props {
        let ast::ObjectProp::KeyValue(key, value) = prop else {
            continue;
        };
        let int = fields
            .iter()
            .find(|f| f.name == key.name)
            .is_some_and(|f| int_view(&t.view(&f.ty), t));
        if int {
            int_position(value, t);
        }
    }
}

/// The arguments for parameters (declaring identifiers `params`) with integer types.
pub(super) fn arguments(params: &[Span], args: &[ast::Expr], t: &mut Typed) {
    for (arg, param) in args.iter().zip(params) {
        let int = t
            .program
            .analysis
            .type_of(*param)
            .is_some_and(|ty| int_view(&t.view(&ty), t));
        if int {
            int_position(arg, t);
        }
    }
}
