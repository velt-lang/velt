//! `(v as Record<symbol, unknown>)[KEY]` (docs/reference/types.md "Record"): a look at a value's
//! symbol-keyed member, as sigx's brand checks do (`isCell`). Velt has no record with symbol
//! keys, so this one form is supported: the read goes through `any`, as `(v as any)[KEY]`
//! does, and gives the record's value type (`unknown`, or `any`). A literal key (`["a"]`) and
//! any other use of such a cast (a member read, a write, a value of its own) are errors.

use velt_common::{Diagnostic, Span};
use velt_syntax::ast;

use super::callback::synth;
use crate::body::{FnCx, Want};
use crate::hir;

impl FnCx<'_, '_> {
    /// `(v as Record<symbol, V>)[index]` read (`object` is the parenthesized cast), `V` being
    /// `unknown` or `any`: `None` when `object` is not such a cast.
    pub(super) fn symbol_record_read(
        &mut self,
        object: &ast::Expr,
        index: &ast::Expr,
        span: Span,
    ) -> Option<hir::Expr> {
        let (expr, ty) = symbol_record_cast(object)?;
        let value = symbol_record_value(ty)?;
        let named = matches!(
            index.kind,
            ast::ExprKind::Lit(_) | ast::ExprKind::Template { .. }
        );
        if named {
            self.cx.error(
                Diagnostic::error(
                    "the key of a `Record<symbol, ...>` must be a symbol (TypeScript's TS2339 for other keys)",
                    index.span,
                )
                .with_note("Velt reads a symbol-keyed member through it: `(v as Record<symbol, unknown>)[KEY]`, with `KEY` a symbol constant"),
            );
            self.expr(expr, None, Want::Borrow);
            return Some(self.error_expr(span));
        }
        let any = ast::TypeExpr {
            kind: ast::TypeExprKind::Named {
                path: vec![ast::Ident {
                    name: "any".into(),
                    span: ty.span,
                }],
                args: vec![],
            },
            span: ty.span,
        };
        let cast = synth(
            ast::ExprKind::Cast {
                expr: Box::new(expr.clone()),
                ty: any,
            },
            object.span,
        );
        let read = synth(
            ast::ExprKind::Index {
                object: Box::new(synth(ast::ExprKind::Paren(Box::new(cast)), object.span)),
                index: Box::new(index.clone()),
                optional: false,
            },
            span,
        );
        let h = self.expr(&read, None, Want::Move);
        let v = self.resolve(value);
        Some(self.coerce(h, v))
    }

    /// `v as Record<symbol, V>` anywhere but in the read above: an error.
    pub(super) fn symbol_record_elsewhere(&mut self, ty: &ast::TypeExpr, span: Span) -> bool {
        if symbol_record_value(ty).is_none() {
            return false;
        }
        self.cx.error(
            Diagnostic::error(
                "a `Record<symbol, ...>` is only supported to read a symbol-keyed member",
                span,
            )
            .with_note("read the member right away: `(v as Record<symbol, unknown>)[KEY]`, with `KEY` a symbol constant; writing one, or keeping the record, is not supported yet"),
        );
        true
    }
}

/// The value and the type of `(v as T)` (in parentheses or not) when `T` is a
/// `Record<symbol, V>` with `V` `unknown` or `any`.
fn symbol_record_cast(e: &ast::Expr) -> Option<(&ast::Expr, &ast::TypeExpr)> {
    match &e.kind {
        ast::ExprKind::Paren(x) => symbol_record_cast(x),
        ast::ExprKind::Cast { expr, ty } if symbol_record_value(ty).is_some() => Some((expr, ty)),
        _ => None,
    }
}

/// `V` when `t` is `Record<symbol, V>` with `V` `unknown` or `any`.
fn symbol_record_value(t: &ast::TypeExpr) -> Option<&ast::TypeExpr> {
    let named = |t: &ast::TypeExpr, names: &[&str]| {
        matches!(&t.kind, ast::TypeExprKind::Named { path, args }
            if args.is_empty() && path.len() == 1 && names.contains(&path[0].name.as_str()))
    };
    let ast::TypeExprKind::Named { path, args } = &t.kind else {
        return None;
    };
    let record = path.len() == 1 && path[0].name == "Record";
    match &args[..] {
        [k, v] if record && named(k, &["symbol"]) && named(v, &["unknown", "any"]) => Some(v),
        _ => None,
    }
}
