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
        E::Cast { ty, .. } => return int_cast(e, ty, cx),
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

/// `5i32`, `1.5f32`, and the same in a literal type: TypeScript has no suffixes. Removing the
/// suffix is a fix only when the literal has the same value as a `number`.
pub(super) fn suffix(lit: &ast::Lit, span: Span, cx: &mut Cx) {
    let (value, suffix) = match lit {
        ast::Lit::Int {
            value,
            suffix: Some(suffix),
        } => (Value::Int(*value), suffix),
        ast::Lit::Float {
            value,
            suffix: Some(suffix),
        } => (Value::Float(*value), suffix),
        _ => return,
    };
    let message = format!(
        "TypeScript has no number literal suffixes: `{}`",
        cx.text(span)
    );
    let why = "the suffix gives the literal a Velt number type; in TypeScript every number \
               literal is a `number` (a 64-bit float)";
    match value.as_number(suffix) {
        Ok(()) => {
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
                message,
                &[why, "remove the suffix"],
                fix,
            );
        }
        Err(differs) => cx.error("number-suffix", span, message, &[why, differs]),
    }
}

/// A suffixed literal's value.
enum Value {
    Int(u128),
    Float(f64),
}

/// 2^53: every integer up to it in magnitude is exactly a `number`.
const MAX_EXACT: u128 = 1 << 53;

impl Value {
    /// `Ok` when the literal, with `suffix`, has the same value as the literal without it (a
    /// `number`); otherwise what to write instead.
    fn as_number(&self, suffix: &str) -> Result<(), &'static str> {
        let number = match *self {
            Value::Int(v) if v > MAX_EXACT => {
                return Err("a `number` holds integers exactly only up to 2^53 \
                            (9007199254740992); TypeScript has no `i64`, so keep code that \
                            needs larger integers out of shared files");
            }
            Value::Int(v) => v as f64,
            Value::Float(v) => v,
        };
        if suffix == "f32" && (number as f32) as f64 != number {
            return Err(
                "an `f32` literal is rounded to 32 bits, so without the suffix it \
                        would be a different value; write the value as a `number`",
            );
        }
        Ok(())
    }
}

/// `x as i64`: Velt converts (truncating toward zero), TypeScript's `as` never changes a value
/// and has no `i64`.
fn int_cast(e: &ast::Expr, ty: &ast::TypeExpr, cx: &mut Cx) -> Option<Span> {
    let ast::TypeExprKind::Named { path, args } = &ty.kind else {
        return None;
    };
    let ([name], []) = (path.as_slice(), args.as_slice()) else {
        return None;
    };
    if !INT_TYPES.contains(&name.name.as_str()) {
        return None;
    }
    cx.error(
        "int-cast",
        e.span,
        format!(
            "`as {}` converts the value in Velt, but not in TypeScript",
            name.name
        ),
        &[
            "TypeScript's `as` only changes the static type and has no integer types; Velt's \
             cast to an integer type truncates toward zero",
            "write `Math.trunc(x)`, which truncates in both, and make the target `number`",
        ],
    );
    Some(ty.span)
}
