//! Rules on expressions: literal suffixes (`number-suffix`), `as` casts to integer types
//! (`int-cast`), named object literals (`struct`) and `throws` on arrows.

use velt_common::Span;
use velt_syntax::ast::{self, ExprKind as E};

use super::types::INT_TYPES;
use super::{decls, Cx};
use crate::Fix;

/// Check one expression (its sub-expressions are visited separately). Returns the target type
/// of an `int-cast`, which the type rules then skip.
pub(super) fn expr(e: &ast::Expr, cx: &mut Cx) -> Option<Span> {
    match &e.kind {
        E::Lit(lit) => suffix(lit, e.span, cx),
        E::Cast { expr, ty } => return int_cast(e, expr, ty, cx),
        E::StructLit { name, .. } => cx.error(
            "struct",
            name.span,
            format!(
                "TypeScript has no named object literals: `{} {{ … }}`",
                cx.text(name.span)
            ),
            &[
                "Velt builds a struct or object type by name; in TypeScript an object literal \
                 takes its type from where it goes",
                "write a plain object literal where its type is known (`const p: P = { … }`)",
            ],
        ),
        E::Arrow {
            throws: Some(throws),
            ..
        } => decls::throws(throws, true, cx),
        _ => {}
    }
    None
}

/// `5i32`, `1.5f32`, and the same in a literal type: TypeScript has no suffixes.
pub(super) fn suffix(lit: &ast::Lit, span: Span, cx: &mut Cx) {
    let (ast::Lit::Int {
        suffix: Some(suffix),
        ..
    }
    | ast::Lit::Float {
        suffix: Some(suffix),
        ..
    }) = lit
    else {
        return;
    };
    let suffix_span = Span::new(
        span.file,
        span.hi.saturating_sub(suffix.len() as u32),
        span.hi,
    );
    let fix = Fix {
        span: suffix_span,
        replacement: String::new(),
        title: format!("remove the suffix `{suffix}`"),
    };
    cx.error_with_fix(
        "number-suffix",
        span,
        format!(
            "TypeScript has no number literal suffixes: `{}`",
            cx.text(span)
        ),
        &[
            "the suffix gives the literal a Velt number type; in TypeScript every number \
             literal is a `number`",
            "remove the suffix",
        ],
        fix,
    );
}

/// `x as i64`: Velt converts (truncating toward zero), TypeScript's `as` never changes a value
/// and has no `i64`.
fn int_cast(e: &ast::Expr, value: &ast::Expr, ty: &ast::TypeExpr, cx: &mut Cx) -> Option<Span> {
    let ast::TypeExprKind::Named { path, args } = &ty.kind else {
        return None;
    };
    let ([name], []) = (path.as_slice(), args.as_slice()) else {
        return None;
    };
    if !INT_TYPES.contains(&name.name.as_str()) {
        return None;
    }
    let replacement = format!("Math.trunc({})", cx.text(value.span));
    let fix = Fix {
        span: e.span,
        replacement: replacement.clone(),
        title: format!("replace with `{replacement}`"),
    };
    cx.error_with_fix(
        "int-cast",
        e.span,
        format!(
            "`as {}` converts the value in Velt, but not in TypeScript",
            name.name
        ),
        &[
            "TypeScript's `as` only changes the static type and has no integer types; Velt's \
             cast to an integer type truncates toward zero",
            "write `Math.trunc(x)`, which truncates in both",
        ],
        fix,
    );
    Some(ty.span)
}
