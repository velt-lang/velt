//! Replacer and reviver callbacks written in place (`JSON.stringify(v, (k, v: JsonValue) => …)`,
//! `value.revive(…)`): until #924 gives `any` its JavaScript meaning, the value parameter must be
//! written `v: JsonValue`. Typed from context it would be a `JsonValue` too, and code testing it
//! as a JavaScript value (`typeof v === "number"`) would compile and quietly do less than in
//! Node; written out, the type says what the callback gets.

use velt_common::Diagnostic;
use velt_syntax::ast;

use crate::body::FnCx;
use crate::hir;

/// Is `t` the type `JsonValue` as written?
fn is_json_value(t: &ast::TypeExpr) -> bool {
    matches!(&t.kind, ast::TypeExprKind::Named { path, args }
        if args.is_empty() && path.last().is_some_and(|i| i.name == "JsonValue"))
}

impl FnCx<'_, '_> {
    /// `a` as the callback of `what` (`` `JSON.stringify` `` or `` `JsonValue.revive` ``): `None`
    /// when `a` is no arrow or function expression; `Some(true)` when its value parameter is
    /// written `JsonValue` (or it takes none), and `Some(false)` when it is not, reported.
    pub(super) fn json_callback_typed(&mut self, a: &ast::Expr, what: &str) -> Option<bool> {
        let value = match &a.kind {
            ast::ExprKind::Arrow { params, .. } => {
                params.get(1).map(|p| (p.name.clone(), p.ty.clone()))
            }
            ast::ExprKind::Function(f) => f
                .sig
                .params
                .get(1)
                .map(|p| (p.name.clone(), Some(p.ty.clone()))),
            _ => return None,
        };
        let Some((name, ty)) = value else {
            return Some(true);
        };
        if ty.as_ref().is_some_and(is_json_value) {
            return Some(true);
        }
        let v = &name.name;
        self.cx.error(
            Diagnostic::error(
                format!("the value parameter of a callback of {what} must be written `{v}: JsonValue` (#924)"),
                name.span,
            )
            .with_note("the callback gets each value as a `JsonValue`, not as a JavaScript value: `typeof`, arithmetic and comparisons on it would not do what they do in Node until `any` has its JavaScript meaning (#924)")
            .with_note(format!("write `(key, {v}: JsonValue) => …` and read the value with `{v}.isNumber()`, `{v}.asNumber()` and the other `JsonValue` methods")),
        );
        Some(false)
    }

    /// `JSON.parse(text, reviver)` with a function written in place: an error until #924 (the
    /// second argument is the options); `None` for any other second argument.
    pub(super) fn json_parse_reviver(
        &mut self,
        a: &ast::Expr,
        span: velt_common::Span,
    ) -> Option<hir::Expr> {
        if !matches!(
            a.kind,
            ast::ExprKind::Arrow { .. } | ast::ExprKind::Function(_)
        ) {
            return None;
        }
        self.cx.error(
            Diagnostic::error(
                "`JSON.parse(text, reviver)` is not supported yet: it needs `any` to have its JavaScript meaning (#924)",
                a.span,
            )
            .with_note("parse the document as a `JsonValue` and revive that: `JSON.parseValue(text).revive((key: string, v: JsonValue) => …)`")
            .with_note("`JSON.parse`'s second argument is its options (`{ maxDepth, comments, trailingCommas }`)"),
        );
        Some(self.error_expr(span))
    }
}
