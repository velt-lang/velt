//! Rules on written types: Velt's number type names (`velt-number-type`), `bool` (`bool-type`),
//! `Promise<T, E>` (`promise-error-type`) and suffixed literal types (`number-suffix`).

use velt_syntax::ast::{self, TypeExprKind as T};

use super::{decls, exprs, Cx};
use crate::Fix;

/// Velt's integer types: `tsc` rejects them, and code written with them gets integer
/// arithmetic (`/` truncates), which `number` doesn't have.
pub(super) const INT_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize",
];

/// Check one type (its nested types are visited separately).
pub(super) fn ty(t: &ast::TypeExpr, cx: &mut Cx) {
    match &t.kind {
        T::Named { path, args } => named(t, path, args, cx),
        T::Literal(lit) => exprs::suffix(&lit.lit, t.span, cx),
        T::Function {
            throws: Some(throws),
            ..
        } => decls::throws(throws, false, cx),
        _ => {}
    }
}

fn named(t: &ast::TypeExpr, path: &[ast::Ident], args: &[ast::TypeExpr], cx: &mut Cx) {
    let [name] = path else { return };
    match (name.name.as_str(), args) {
        ("Promise", [value, _]) => promise_error(t, value, cx),
        ("f64", []) => f64_type(t, cx),
        ("f32", []) => cx.error(
            "velt-number-type",
            t.span,
            "`f32` is not a TypeScript type".into(),
            &[
                "TypeScript has one number type, `number` (a 64-bit float); Velt's `f32` is a \
                 32-bit float, which rounds differently",
                "write `number`",
            ],
        ),
        ("bool", []) => bool_type(t, cx),
        (int, []) if INT_TYPES.contains(&int) => cx.error(
            "velt-number-type",
            t.span,
            format!("`{int}` is not a TypeScript type"),
            &[
                "TypeScript has one number type, `number` (a 64-bit float); Velt's integer \
                 types are machine integers, so `/` truncates and the range is limited",
                "write `number`",
            ],
        ),
        _ => {}
    }
}

fn f64_type(t: &ast::TypeExpr, cx: &mut Cx) {
    let fix = Fix {
        span: t.span,
        replacement: "number".into(),
        title: "replace with `number`".into(),
    };
    cx.error_with_fix(
        "velt-number-type",
        t.span,
        "`f64` is not a TypeScript type".into(),
        &[
            "`f64` is Velt's other name for `number`, the only name TypeScript knows",
            "write `number`",
        ],
        fix,
    );
}

fn bool_type(t: &ast::TypeExpr, cx: &mut Cx) {
    let fix = Fix {
        span: t.span,
        replacement: "boolean".into(),
        title: "replace with `boolean`".into(),
    };
    cx.error_with_fix(
        "bool-type",
        t.span,
        "`bool` is not a TypeScript type".into(),
        &[
            "`bool` is Velt's other name for `boolean`, the only name TypeScript knows",
            "write `boolean`",
        ],
        fix,
    );
}

/// `Promise<T, E>`: TypeScript's `Promise` takes one type argument.
fn promise_error(t: &ast::TypeExpr, value: &ast::TypeExpr, cx: &mut Cx) {
    let replacement = format!("Promise<{}>", cx.text(value.span));
    let fix = Fix {
        span: t.span,
        replacement: replacement.clone(),
        title: format!("replace with `{replacement}`"),
    };
    cx.error_with_fix(
        "promise-error-type",
        t.span,
        "TypeScript's `Promise` takes one type argument".into(),
        &[
            "Velt's `Promise<T, E>` says the promise rejects with `E`; TypeScript doesn't track \
             what a promise rejects with",
            "write `Promise<T>`: Velt infers what an `async` function throws",
        ],
        fix,
    );
}
